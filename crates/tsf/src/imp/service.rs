//! 输入法对象：每个程序的每个界面线程一个。接收按键、问引擎、改文档里的字。
//!
//! 和别的程序和平相处的规矩（详见 docs/方案.md「和别的程序不冲突」）：
//! - 没在打字就什么都不显示、不拦任何键；Ctrl/Alt/Win 组合键、修饰键一律放过；
//!   功能键、音量媒体键、截屏键、菜单键任何时候都放过
//! - 按下和松开成对：程序收到了一个键的按下，就一定收到它的松开（见 gate.rs）。
//!   否则网页、游戏以为这个键一直按着（B 站长按方向键开倍速、空格不暂停、游戏人物一直跑）
//! - 引擎里的拼音和屏幕上的永远一致：这里开始不了输入（不是文字框），立刻清掉引擎里的拼音，
//!   不会出现"屏幕上什么都没有，空格、方向键却被输入法拿走"
//! - 下面这些情况所有键原样交给程序：没有输入框、只读、密码框、数字框、程序关掉了输入法、
//!   引擎没运行或卡住（超过 0.3 秒）
//! - 全屏游戏里默认不打字（按键全交给游戏）；单按一下 Shift 才临时打中文，回车或 Esc 发出去后自动恢复。
//!   "全屏"只看正在打字的这个窗口自己有没有铺满它所在的屏幕（见 screen.rs），接了副屏也不会互相影响
//! - 任何出错都兜住，绝不让程序崩溃

use std::cell::RefCell;
use std::time::{Duration, Instant};

use pinyin_ipc::{Reply, Request};
use pinyin_types::{Key, Mods, View};
use windows::core::{implement, ComObject, IUnknownImpl, Interface, Ref, Result, BOOL, GUID};
use windows::Win32::Foundation::{LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONULL};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::TextServices::{
    ITfCompartmentMgr, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfDocumentMgr,
    ITfEditSession, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource, ITfTextInputProcessorEx,
    ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl, ITfThreadMgr, ITfThreadMgrEventSink,
    ITfThreadMgrEventSink_Impl, GUID_COMPARTMENT_EMPTYCONTEXT, GUID_COMPARTMENT_KEYBOARD_DISABLED,
    GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, TF_ES_ASYNCDONTCARE, TF_ES_READ, TF_ES_READWRITE, TF_ES_SYNC,
    TS_SD_INPUTPANEMANUALDISPLAYENABLE, TS_SD_READONLY, TS_SS_TRANSITORY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetGUIThreadInfo, GetWindowRect, IsIconic, IsWindowVisible, IsZoomed, GA_ROOT, GUITHREADINFO,
};

use windows::Win32::UI::Input::KeyboardAndMouse::VK_SHIFT;
use windows::Win32::UI::TextServices::{TF_MOD_ON_KEYUP, TF_MOD_SHIFT, TF_PRESERVEDKEY};

use super::client::EngineClient;
use super::edit::{Action, EditSession};
use super::keys::{self, ShiftTracker};
use super::{candwin, debug, guard};
use crate::fields::Kind;
use crate::gate::{self, KeyGate};
use crate::screen;

#[derive(Default)]
struct State {
    tm: Option<ITfThreadMgr>,
    tid: u32,
    tm_cookie: Option<u32>,
    key_sink: bool,
    /// 正在打的拼音（文档里那段带下划线的字）
    composition: Option<ITfComposition>,
    comp_ctx: Option<ITfContext>,
    client: EngineClient,
    shift: ShiftTracker,
    /// 按下和松开配对
    gate: KeyGate,
    /// 引擎里有没打完的拼音（看最近一次回复）
    engine_busy: bool,
    /// 请求了"开始一段输入"、程序还没执行（排着队）：请求的时间
    start_pending: Option<Instant>,
    /// 记住的输入框：在这里开始过输入（好），或者开始不了（不是文字框，比如网页上的视频）
    ctx_marks: Vec<CtxMark>,
    /// 全屏游戏里按 Shift 临时打开了中文
    game_chinese: bool,
    /// 输入框类型（密码框、隐私输入框……）的检查结果：同一个输入框短时间内不重复查
    field_cache: Option<FieldCheck>,
    /// 现在在密码框里：诊断日志连"按了退格、回车"都不记（免得看出密码有几位）
    secret_box: bool,
    /// 上一次写进诊断日志的"不打中文"原因（同一个原因不重复记，免得一个键一行）
    last_block_log: Option<&'static str>,
    /// 已经告诉引擎的"隐私输入框"状态，和那时是第几次连上引擎
    told_private: Option<(bool, u64)>,
    /// 上一次中英切换的时间：系统的"单按 Shift"和自己的检测可能都报一次，只切一次
    last_toggle: Option<Instant>,
    preserved: bool,
}

/// 系统帮我们检测"单独按一下 Shift"（和微软示例输入法一样用"保留键"）。
/// 登记两种写法：有的程序松开 Shift 时还算 Shift 按着，有的不算。
const PRESERVE_SHIFT: GUID = GUID::from_u128(0x6a1d2c4e_83b7_4f0a_9c55_1e2f3a4b5c6d);
const PRESERVE_SHIFT2: GUID = GUID::from_u128(0x6a1d2c4e_83b7_4f0a_9c55_1e2f3a4b5c6e);

fn preserved_keys() -> [(GUID, TF_PRESERVEDKEY); 2] {
    let vk = VK_SHIFT.0 as u32;
    [
        (PRESERVE_SHIFT, TF_PRESERVEDKEY { uVKey: vk, uModifiers: TF_MOD_ON_KEYUP | TF_MOD_SHIFT }),
        (PRESERVE_SHIFT2, TF_PRESERVEDKEY { uVKey: vk, uModifiers: TF_MOD_ON_KEYUP }),
    ]
}

/// 两次报告之间不超过这么久，算同一次按键
const SAME_TAP: Duration = Duration::from_millis(400);

/// 排队的"开始输入"请求最多等这么久，还没执行就当作失败
const PENDING: Duration = Duration::from_secs(2);

/// "这里开始不了输入"记多久（换了输入框也会忘掉）
const DEAD_FOR: Duration = Duration::from_secs(60);

/// 输入框类型查一次管多久（换了输入框马上重查）
const FIELD_TTL: Duration = Duration::from_secs(1);

struct FieldCheck {
    ctx: ITfContext,
    kind: Kind,
    at: Instant,
}

struct CtxMark {
    ctx: ITfContext,
    /// true：在这里成功开始过输入；false：开始不了
    good: bool,
    at: Instant,
}

fn same_ctx(a: &ITfContext, b: &ITfContext) -> bool {
    a.as_raw() == b.as_raw()
}

fn mark_ctx(st: &mut State, ctx: &ITfContext, good: bool) {
    if let Some(m) = st.ctx_marks.iter_mut().find(|m| same_ctx(&m.ctx, ctx)) {
        // 成功输入过的输入框，不会因为一次失败（程序正忙）就被当成不能打字
        if !(m.good && !good) {
            m.good = good;
            m.at = Instant::now();
        }
        return;
    }
    if st.ctx_marks.len() >= 16 {
        st.ctx_marks.remove(0);
    }
    st.ctx_marks.push(CtxMark { ctx: ctx.clone(), good, at: Instant::now() });
}

/// 诊断日志：只记不会打出字的键（空格、回车、方向键……），绝不记打的字。
fn write_key_log(vk: u32, what: &str, eaten: bool) {
    if debug::enabled() {
        if let Some(name) = gate::key_label(vk) {
            debug::log(&format!("{what} {name}：{}", if eaten { "输入法用了" } else { "交给程序" }));
        }
    }
}

/// 一次"编辑会话"请求的结果
pub struct Edited {
    /// 已经执行了（同步执行）
    pub ran: bool,
    /// 程序接受了（已经执行，或者排上队了）
    pub accepted: bool,
    /// 请求本身就出错了（输入框已经不在了之类）
    pub error: bool,
    /// 输入框类型检查的结果
    pub kind: Kind,
}

#[implement(ITfTextInputProcessorEx, ITfKeyEventSink, ITfThreadMgrEventSink, ITfCompositionSink)]
pub struct TextService {
    st: RefCell<State>,
}

impl TextService {
    pub fn new() -> Self {
        TextService { st: RefCell::new(State::default()) }
    }

    pub fn tid(&self) -> u32 {
        self.st.borrow().tid
    }

    pub fn composition(&self) -> Option<ITfComposition> {
        self.st.borrow().composition.clone()
    }

    pub fn set_composition(&self, c: Option<ITfComposition>, ctx: Option<ITfContext>) {
        let mut st = self.st.borrow_mut();
        if c.is_some() {
            st.start_pending = None;
            if let Some(x) = &ctx {
                mark_ctx(&mut st, x, true);
            }
        }
        st.composition = c;
        st.comp_ctx = ctx;
    }

    /// 这里开始不了输入（网页上没有输入框的地方、不接受文字的控件）：
    /// 清掉引擎里的拼音，收起候选窗，记住这个地方（接下来的按键直接交给程序）。
    pub fn compose_failed(&self, ctx: &ITfContext) {
        debug::log("这里开始不了输入（不是文字框）：清掉引擎里的拼音，按键交给程序");
        candwin::hide();
        {
            let mut st = self.st.borrow_mut();
            st.start_pending = None;
            mark_ctx(&mut st, ctx, false);
        }
        self.reset_engine();
    }

    /// 清空引擎里正在打的拼音（上文保留）。
    fn reset_engine(&self) {
        self.notify(&Request::Reset);
        self.st.borrow_mut().engine_busy = false;
    }

    fn ctx_dead(&self, ctx: &ITfContext) -> bool {
        self.st.borrow().ctx_marks.iter().any(|m| same_ctx(&m.ctx, ctx) && !m.good && m.at.elapsed() < DEAD_FOR)
    }

    fn composing(&self) -> bool {
        self.st.borrow().composition.is_some()
    }

    fn call(&self, req: &Request) -> Option<Reply> {
        let mut st = self.st.borrow_mut();
        let r = st.client.call(req);
        if r.is_none() {
            // 连接断了：重新连上时引擎那边是全新的，没有拼音
            st.engine_busy = false;
        }
        r
    }

    /// 把一个按键交给引擎处理，记下引擎里还有没有拼音。
    fn key_call(&self, key: Key, mods: Mods) -> Option<(bool, Option<String>, View)> {
        match self.call(&Request::Key { key, mods }) {
            Some(Reply::KeyResult { consumed, commit, view }) => {
                self.st.borrow_mut().engine_busy = view.composing;
                Some((consumed, commit, view))
            }
            _ => None,
        }
    }

    /// 只在已经连上引擎时才发（切换焦点之类的通知不值得为它启动引擎）。
    fn notify(&self, req: &Request) {
        let mut st = self.st.borrow_mut();
        if st.client.connected() {
            let _ = st.client.call(req);
        }
    }
}

impl Default for TextService {
    fn default() -> Self {
        Self::new()
    }
}

fn compartment(mgr: &ITfCompartmentMgr, g: &GUID) -> Option<i32> {
    unsafe {
        let v: VARIANT = mgr.GetCompartment(g).ok()?.GetValue().ok()?;
        if v.is_empty() {
            None
        } else {
            i32::try_from(&v).ok()
        }
    }
}

/// 这个线程有没有系统光标（普通的文字输入框都有；游戏画面一般没有）。
fn has_system_caret() -> bool {
    unsafe {
        let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        GetGUIThreadInfo(GetCurrentThreadId(), &mut gti).is_ok() && !gti.hwndCaret.is_invalid()
    }
}

fn to_rect(r: RECT) -> screen::Rect {
    screen::Rect::new(r.left, r.top, r.right, r.bottom)
}

/// 正在打字的这个窗口（最外层那个）是不是铺满了它自己所在的那块屏幕。
/// 只看这一个窗口：别的屏幕上有没有全屏程序和这里无关。
fn window_fills_screen(ctx: &ITfContext) -> bool {
    unsafe {
        let hwnd =
            ctx.GetActiveView().and_then(|v| v.GetWnd()).ok().filter(|h| !h.is_invalid()).unwrap_or_else(|| GetFocus());
        if hwnd.is_invalid() {
            return false;
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        if root.is_invalid() || !IsWindowVisible(root).as_bool() || IsIconic(root).as_bool() {
            return false;
        }
        let mut wr = RECT::default();
        if GetWindowRect(root, &mut wr).is_err() {
            return false;
        }
        let mon = MonitorFromWindow(root, MONITOR_DEFAULTTONULL);
        if mon.is_invalid() {
            return false;
        }
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return false;
        }
        screen::fills_screen(to_rect(wr), to_rect(mi.rcMonitor), IsZoomed(root).as_bool())
    }
}

impl TextService_Impl {
    fn edit(&self, ctx: &ITfContext, action: Action, sync: bool) -> Edited {
        let tid = self.tid();
        let es = ComObject::new(EditSession::new(ctx.clone(), self.to_object(), action));
        let i: ITfEditSession = es.to_interface();
        let check = matches!(es.action, Action::CheckScope);
        let flags = match (check, sync) {
            (true, _) => TF_ES_SYNC | TF_ES_READ,
            (false, true) => TF_ES_SYNC | TF_ES_READWRITE,
            (false, false) => TF_ES_ASYNCDONTCARE | TF_ES_READWRITE,
        };
        let mut r = unsafe { ctx.RequestEditSession(tid, &i, flags) };
        if !r.as_ref().is_ok_and(|hr| hr.is_ok()) && sync && !check {
            // 程序这会儿不让立刻改：排队稍后改（字不会丢）
            r = unsafe { ctx.RequestEditSession(tid, &i, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE) };
        }
        Edited {
            ran: es.ran.get(),
            accepted: r.as_ref().is_ok_and(|hr| hr.is_ok()),
            error: r.is_err(),
            kind: es.kind.get(),
        }
    }

    /// 结束正在打的拼音（字留在文档里），比如切换了输入框。
    fn end_composition(&self) {
        let (comp, ctx) = {
            let mut st = self.st.borrow_mut();
            (st.composition.take(), st.comp_ctx.take())
        };
        if let (Some(comp), Some(ctx)) = (comp, ctx) {
            // 编辑会话可能稍后才执行：把要结束的那段直接交给它
            self.edit(&ctx, Action::End(comp), false);
        }
        candwin::hide();
    }

    /// 这个输入框是哪一种：能正常打字、隐私输入框（打字但不记），还是输入法不该碰（原因给诊断日志）。
    /// 全在这个程序里判断，不问引擎。
    fn field(&self, ctx: &ITfContext) -> Kind {
        // 刚试过，这里开始不了输入
        if self.ctx_dead(ctx) {
            return Kind::Off("刚试过这里开始不了输入");
        }
        // 输入法被"关上"了（Ctrl+空格、程序关的，开机时系统有时也这样）：单按 Shift 能重新打开
        if self.keyboard_closed() {
            return Kind::Off("输入法被关上了（Ctrl+空格、程序或系统关的；单按 Shift 打开）");
        }
        unsafe {
            // 程序禁用了输入法（很多游戏、密码框这样做）
            if let Ok(m) = ctx.cast::<ITfCompartmentMgr>() {
                for g in [GUID_COMPARTMENT_KEYBOARD_DISABLED, GUID_COMPARTMENT_EMPTYCONTEXT] {
                    if compartment(&m, &g).is_some_and(|v| v != 0) {
                        return Kind::Off("程序在这里禁用了输入法");
                    }
                }
            }
            if let Ok(s) = ctx.GetStatus() {
                if s.dwDynamicFlags & TS_SD_READONLY != 0 {
                    return Kind::Off("只读");
                }
            }
        }
        // 输入框声明的类型：密码框、数字框、隐私输入框（同一个输入框短时间内用上次的结果）
        {
            let st = self.st.borrow();
            if let Some(c) = st.field_cache.as_ref().filter(|c| same_ctx(&c.ctx, ctx) && c.at.elapsed() < FIELD_TTL) {
                return c.kind;
            }
        }
        let e = self.edit(ctx, Action::CheckScope, true);
        let kind = if e.error { Kind::Off("读不了这个输入框（已经没了）") } else { e.kind };
        // 程序这会儿不让读（没执行）：不记，下一个键再查
        if e.ran || e.error {
            self.st.borrow_mut().field_cache = Some(FieldCheck { ctx: ctx.clone(), kind, at: Instant::now() });
        }
        kind
    }

    /// 输入法这会儿是不是被"关上"了（系统的开关，和我们自己的中/英不是一回事）：关着时按键全部原样输入。
    fn keyboard_closed(&self) -> bool {
        let tm = self.st.borrow().tm.clone();
        tm.and_then(|tm| tm.cast::<ITfCompartmentMgr>().ok())
            .is_some_and(|m| compartment(&m, &GUID_COMPARTMENT_KEYBOARD_OPENCLOSE) == Some(0))
    }

    /// 把输入法"打开"（激活时、用户单按 Shift 要中文时）。返回设置成功没有。
    fn open_keyboard(&self) -> bool {
        let (tm, tid) = {
            let st = self.st.borrow();
            (st.tm.clone(), st.tid)
        };
        let Some(tm) = tm else { return false };
        unsafe {
            tm.cast::<ITfCompartmentMgr>()
                .ok()
                .and_then(|m| m.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE).ok())
                .is_some_and(|c| c.SetValue(tid, &VARIANT::from(1i32)).is_ok())
        }
    }

    /// 告诉引擎现在是不是隐私输入框（状态变了、或者重新连上了引擎才发）。返回引擎知道了没有。
    fn tell_private(&self, on: bool) -> bool {
        let up_to_date = {
            let st = self.st.borrow();
            st.client.connected() && st.told_private == Some((on, st.client.generation()))
        };
        if up_to_date {
            return true;
        }
        if let Some(Reply::Ok) = self.call(&Request::SetPrivate { on }) {
            let mut st = self.st.borrow_mut();
            let g = st.client.generation();
            st.told_private = Some((on, g));
            if on {
                debug::log("隐私输入框（无痕窗口等）：照常打字，什么都不记");
            }
            return true;
        }
        false
    }

    /// 按键写进诊断日志（密码框里连退格、回车都不记）。
    fn log_key(&self, vk: u32, what: &str, eaten: bool) {
        if !self.st.borrow().secret_box {
            write_key_log(vk, what, eaten);
        }
    }

    /// "不打中文"的原因写进诊断日志：同一个原因只记一次（免得每个键一行，看出打了几个字）。
    fn log_block(&self, why: &'static str) {
        let new = self.st.borrow().last_block_log != Some(why);
        if new {
            self.st.borrow_mut().last_block_log = Some(why);
            debug::log(&format!("不打中文：{why}"));
        }
    }

    /// 全屏游戏：打字的这个窗口铺满了它所在的屏幕，又不是真正的文字输入框（见 screen.rs）。
    fn in_game(&self, ctx: &ITfContext) -> bool {
        let status = unsafe { ctx.GetStatus() }.ok();
        let transitory = status.is_some_and(|s| s.dwStaticFlags & TS_SS_TRANSITORY != 0);
        let app_text_box = status.is_some_and(|s| s.dwDynamicFlags & TS_SD_INPUTPANEMANUALDISPLAYENABLE != 0);
        // 便宜的先看：大多数输入框到这里就知道不是游戏了
        if !transitory || app_text_box {
            return false;
        }
        let caret = has_system_caret();
        if caret {
            return false;
        }
        screen::looks_like_game(screen::Signals {
            fills_screen: window_fills_screen(ctx),
            transitory,
            app_text_box,
            caret,
        })
    }

    /// 引擎里有拼音、屏幕上却没有（开始输入失败了，或者程序一直没执行我们的请求）：清掉。
    /// 不然引擎以为还在打字，空格、方向键都会被拿走，而用户什么也看不到。
    fn drop_hidden_input(&self) {
        let stale = {
            let st = self.st.borrow();
            st.engine_busy && !st.start_pending.is_some_and(|t| t.elapsed() < PENDING)
        };
        if stale {
            debug::log("引擎里有屏幕上看不到的拼音：清掉");
            candwin::hide();
            self.st.borrow_mut().start_pending = None;
            self.reset_engine();
        }
    }

    /// 一次按下要不要（只判断，真正处理在 key_down）。
    fn decide_down(&self, ctx: Option<ITfContext>, vk: u32, lp: isize) -> bool {
        if keys::is_shift(vk) {
            let repeat = lp & (1 << 30) != 0;
            self.st.borrow_mut().shift.shift_down(repeat);
            if !repeat {
                debug::log("Shift 按下");
            }
            return false;
        }
        self.st.borrow_mut().shift.other_key();
        let Some(ctx) = ctx else { return false };
        let Some(key) = keys::translate(vk, lp) else { return false };
        let mods = keys::mods();
        let composing = self.composing();
        if composing {
            // 打到一半按了快捷键（Ctrl+S、Alt+Tab……）：先把打的字母原样上屏，再把快捷键交给程序。
            // 在这里就做完，不等 OnKeyDown：按键一旦说了"要"，老式程序就收不到原来的键了
            if mods.ctrl || mods.alt {
                match self.key_call(key, mods) {
                    Some((_, commit, view)) => {
                        self.edit(&ctx, Action::Update { commit, view, flash: None }, true);
                    }
                    _ => self.end_composition(),
                }
                return false;
            }
        } else {
            // 先在这个程序里看这里该不该打字。不该的话，这个键一个字都不发给引擎：
            // 密码框里打的字绝不离开这个程序
            let kind = self.field(&ctx);
            self.st.borrow_mut().secret_box = kind == Kind::Secret;
            match kind {
                Kind::Secret => {
                    self.log_block("密码框或 PIN");
                    return false;
                }
                Kind::Off(why) => {
                    self.log_block(why);
                    return false;
                }
                Kind::Normal | Kind::Private => {}
            }
            if self.in_game(&ctx) && !self.st.borrow().game_chinese {
                self.log_block("全屏游戏里（单按 Shift 临时打开中文）");
                return false;
            }
            self.st.borrow_mut().last_block_log = None;
            // 隐私输入框：引擎先得知道"这里什么都不记"，不然不打中文
            if !self.tell_private(kind == Kind::Private) {
                return false;
            }
            self.drop_hidden_input();
            if keys::is_numpad(vk) {
                self.pass_through(key, mods);
                return false;
            }
        }
        match self.call(&Request::TestKey { key, mods }) {
            Some(Reply::Bool(true)) => true,
            Some(Reply::Bool(false)) => {
                if composing {
                    // 屏幕上还有拼音，引擎却不接这个键了（引擎重启过、别的程序里切成了纯英文）：
                    // 结束这段，字母留在原处
                    self.end_composition();
                    self.reset_engine();
                }
                self.pass_through(key, mods);
                false
            }
            _ => {
                // 引擎没了：结束输入，按键原样交给程序
                if composing {
                    self.end_composition();
                }
                false
            }
        }
    }

    /// 放过的键也告诉引擎一声：打了数字、回车、方向键之后上文就变了。
    fn pass_through(&self, key: Key, mods: Mods) {
        if let Some((true, ..)) = self.key_call(key, mods) {
            // 两边不一致（不该发生）：丢掉引擎里的状态
            self.reset_engine();
        }
        if matches!(key, Key::Enter | Key::Escape) {
            // 游戏里聊天发出去了 / 关掉了：恢复成按键全交给游戏
            self.st.borrow_mut().game_chinese = false;
        }
    }

    /// 真正处理一个说了"要"的按键。返回最后有没有用掉它。
    fn key_down(&self, ctx: Option<ITfContext>, vk: u32, lp: isize) -> bool {
        let Some(ctx) = ctx else { return false };
        let Some(key) = keys::translate(vk, lp) else { return false };
        let mods = keys::mods();
        let was_composing = self.composing();
        match self.key_call(key, mods) {
            Some((consumed, commit, view)) => {
                let starting = !was_composing && view.composing;
                if consumed || commit.is_some() || was_composing {
                    let e = self.edit(&ctx, Action::Update { commit, view, flash: None }, false);
                    if starting {
                        if e.ran {
                            if !self.composing() {
                                // 开始不了（compose_failed 已经清掉了引擎里的拼音）：这个键还给程序
                                return false;
                            }
                        } else if e.accepted {
                            self.st.borrow_mut().start_pending = Some(Instant::now());
                        } else {
                            self.compose_failed(&ctx);
                            return false;
                        }
                    }
                }
                consumed
            }
            None => {
                if was_composing {
                    self.end_composition();
                }
                false
            }
        }
    }

    // ---- 按下、松开的四个入口：每次按键只判断一次，按下和松开成对（见 gate.rs） ----

    fn on_test_key_down(&self, ctx: Option<ITfContext>, vk: u32, lp: isize) -> bool {
        let now = Instant::now();
        let cached = self.st.borrow_mut().gate.test_down_cached(vk, lp, now);
        if let Some(eat) = cached {
            return eat;
        }
        let eat = self.decide_down(ctx, vk, lp);
        self.st.borrow_mut().gate.tested_down(vk, lp, eat, now);
        self.log_key(vk, "按下（问）", eat);
        eat
    }

    fn on_key_down(&self, ctx: Option<ITfContext>, vk: u32, lp: isize) -> bool {
        let now = Instant::now();
        let cached = self.st.borrow_mut().gate.key_down_cached(vk, lp, now);
        let eat = match cached {
            Some(e) => e,
            None => self.decide_down(ctx.clone(), vk, lp),
        };
        let eaten = eat && self.key_down(ctx, vk, lp);
        self.st.borrow_mut().gate.finished_down(vk, eaten);
        if cached.is_none() || eaten != eat {
            self.log_key(vk, "按下", eaten);
        }
        eaten
    }

    fn on_test_key_up(&self, ctx: Option<ITfContext>, vk: u32) -> bool {
        let now = Instant::now();
        let cached = self.st.borrow_mut().gate.up_cached(vk, now, false);
        if let Some(eat) = cached {
            return eat;
        }
        let eat = self.decide_up(ctx, vk);
        self.st.borrow_mut().gate.tested_up(vk, eat, now);
        self.log_key(vk, "松开（问）", eat);
        eat
    }

    fn on_key_up(&self, ctx: Option<ITfContext>, vk: u32) -> bool {
        let now = Instant::now();
        let cached = self.st.borrow_mut().gate.up_cached(vk, now, true);
        match cached {
            Some(eat) => eat,
            None => {
                let eat = self.decide_up(ctx, vk);
                self.log_key(vk, "松开", eat);
                eat
            }
        }
    }

    /// 一次松开要不要：只有按下被输入法用掉的键才要。
    fn decide_up(&self, ctx: Option<ITfContext>, vk: u32) -> bool {
        if keys::is_shift(vk) {
            let (desc, tap) = {
                let mut st = self.st.borrow_mut();
                let d = st.shift.describe();
                (d, st.shift.shift_up())
            };
            let dup = self.recently_toggled();
            debug::log(&format!("Shift 松开（按键接口）：{desc} → 算单按={tap} 刚切过={dup}"));
            if tap && !dup {
                self.shift_tap(ctx);
            }
            // Shift 松开永远交给程序
            return false;
        }
        self.st.borrow_mut().gate.decide_up(vk)
    }

    fn recently_toggled(&self) -> bool {
        self.st.borrow().last_toggle.is_some_and(|t| t.elapsed() < SAME_TAP)
    }

    /// 系统报告"单独按了一下 Shift"（保留键）。
    fn preserved_shift(&self, ctx: Option<ITfContext>) {
        if self.recently_toggled() {
            debug::log("Shift 松开（系统保留键）：刚切过，忽略");
            return;
        }
        let (desc, preview) = {
            let mut st = self.st.borrow_mut();
            let d = st.shift.describe();
            let p = st.shift.preview();
            // 这次按键由这里处理了：之后按键接口再报松开，不再算
            st.shift.reset();
            (d, p)
        };
        debug::log(&format!("Shift 松开（系统保留键）：{desc}"));
        // 自己的检测看到了这次按下，而且看到中间点了鼠标或按了别的键：不算
        if preview == Some(false) {
            return;
        }
        self.shift_tap(ctx);
    }

    /// 单按一下 Shift：中文 ↔ 纯英文；在光标旁边闪一下"中"或"英"。
    fn shift_tap(&self, ctx: Option<ITfContext>) {
        self.st.borrow_mut().last_toggle = Some(Instant::now());
        let composing = self.composing();
        // 输入法被"关上"了（开机时系统有时这样，也可能是 Ctrl+空格 或程序关的）：这时打出来本来就是英文，
        // 单按 Shift 就是要中文。把它打开、切到中文——不然屏幕上闪了"中"，打出来却还是英文
        if !composing && self.keyboard_closed() {
            let opened = self.open_keyboard();
            let engine = matches!(self.call(&Request::SetEnglishMode { on: false }), Some(Reply::Ok));
            debug::log(&format!("输入法原来是关上的：打开={opened}，引擎切到中文={engine}"));
            // 在游戏里：就是要临时打中文
            let game = ctx.as_ref().is_some_and(|c| self.in_game(c));
            {
                let mut st = self.st.borrow_mut();
                st.field_cache = None;
                st.last_block_log = None;
                if game {
                    st.game_chinese = true;
                }
                if let Some(ctx) = &ctx {
                    st.ctx_marks.retain(|m| m.good || !same_ctx(&m.ctx, ctx));
                }
            }
            if let Some(ctx) = &ctx {
                self.edit(ctx, Action::Flash(if engine { "中" } else { "启动中" }), false);
            }
            return;
        }
        if let Some(ctx) = &ctx {
            if !composing && self.in_game(ctx) {
                // 游戏里：临时打开 / 关上中文，不动全局的中英状态
                let on = {
                    let mut st = self.st.borrow_mut();
                    st.game_chinese = !st.game_chinese;
                    st.game_chinese
                };
                if on {
                    self.notify(&Request::SetEnglishMode { on: false });
                }
                self.edit(ctx, Action::Flash(if on { "中" } else { "英" }), false);
                return;
            }
        }
        match self.key_call(Key::ShiftTap, Mods::default()) {
            Some((_, commit, view)) => {
                debug::log(&format!("切换成 {}", if view.english_mode { "纯英文" } else { "中文" }));
                let flash = if view.english_mode { "英" } else { "中" };
                if let (false, Some(ctx)) = (view.english_mode, &ctx) {
                    // 切到了中文：说明想在这里打中文。之前记下的"这里开始不了输入"作废，下一个键重新试，
                    // 免得屏幕上闪了"中"，打出来却还是英文
                    self.st.borrow_mut().ctx_marks.retain(|m| m.good || !same_ctx(&m.ctx, ctx));
                }
                if let Some(ctx) = &ctx {
                    self.edit(ctx, Action::Update { commit, view, flash: Some(flash) }, false);
                }
            }
            _ => {
                debug::log("切换失败：连不上引擎（可能还在启动）");
                if composing {
                    self.end_composition();
                }
                // 刚开机引擎还在启动：告诉用户稍等，而不是什么反应都没有
                self.st.borrow_mut().client.start_engine_if_needed();
                if let Some(ctx) = &ctx {
                    self.edit(ctx, Action::Flash("启动中"), false);
                }
            }
        }
    }

    fn focus_changed(&self) {
        self.end_composition();
        self.reset_engine();
        self.notify(&Request::ClearContext);
        let mut st = self.st.borrow_mut();
        st.game_chinese = false;
        st.gate.clear();
        st.start_pending = None;
        // 换了地方："开始不了输入"的记录作废，回来时再试一次；输入框类型重新查
        st.ctx_marks.retain(|m| m.good);
        st.field_cache = None;
        st.secret_box = false;
        st.last_block_log = None;
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        ITfTextInputProcessorEx_Impl::ActivateEx(self, ptim, tid, 0)
    }

    fn Deactivate(&self) -> Result<()> {
        guard(
            || {
                self.end_composition();
                candwin::destroy();
                let (tm, tid, cookie, key_sink, preserved) = {
                    let mut st = self.st.borrow_mut();
                    st.shift.reset();
                    (
                        st.tm.take(),
                        st.tid,
                        st.tm_cookie.take(),
                        std::mem::take(&mut st.key_sink),
                        std::mem::take(&mut st.preserved),
                    )
                };
                if let Some(tm) = tm {
                    unsafe {
                        if let Ok(km) = tm.cast::<ITfKeystrokeMgr>() {
                            if preserved {
                                for (g, k) in preserved_keys() {
                                    let _ = km.UnpreserveKey(&g, &k);
                                }
                            }
                            if key_sink {
                                let _ = km.UnadviseKeyEventSink(tid);
                            }
                        }
                        if let Some(c) = cookie {
                            if let Ok(src) = tm.cast::<ITfSource>() {
                                let _ = src.UnadviseSink(c);
                            }
                        }
                    }
                }
                self.reset_engine();
                let mut st = self.st.borrow_mut();
                st.client.disconnect();
                st.gate.clear();
                st.ctx_marks.clear();
                st.start_pending = None;
                st.field_cache = None;
                st.secret_box = false;
                st.told_private = None;
                Ok(())
            },
            Ok(()),
        )
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(&self, ptim: Ref<ITfThreadMgr>, tid: u32, _flags: u32) -> Result<()> {
        guard(
            || {
                let tm = ptim.ok()?.clone();
                {
                    let mut st = self.st.borrow_mut();
                    st.tm = Some(tm.clone());
                    st.tid = tid;
                }
                unsafe {
                    let km: ITfKeystrokeMgr = tm.cast()?;
                    km.AdviseKeyEventSink(tid, &self.to_interface::<ITfKeyEventSink>(), true)?;
                    self.st.borrow_mut().key_sink = true;
                    let desc: Vec<u16> = "中/英切换".encode_utf16().collect();
                    let mut ok = 0;
                    for (g, k) in preserved_keys() {
                        if km.PreserveKey(tid, &g, &k, &desc).is_ok() {
                            ok += 1;
                        }
                    }
                    self.st.borrow_mut().preserved = ok > 0;
                    debug::log(&format!("输入法激活；系统 Shift 检测登记了 {ok}/2 个"));
                    let src: ITfSource = tm.cast()?;
                    let cookie =
                        src.AdviseSink(&ITfThreadMgrEventSink::IID, &self.to_interface::<ITfThreadMgrEventSink>())?;
                    self.st.borrow_mut().tm_cookie = Some(cookie);
                }
                // 打开输入法（和微软的示例一样）；程序或 Ctrl+空格 可以再关掉，单按 Shift 又能打开
                let opened = self.open_keyboard();
                debug::log(&format!("打开输入法={opened}，现在关着={}", self.keyboard_closed()));
                // 刚开机、刚登录时引擎可能还没运行：现在就启动，不等第一次按键
                self.st.borrow_mut().client.start_engine_if_needed();
                Ok(())
            },
            Ok(()),
        )
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, foreground: BOOL) -> Result<()> {
        guard(
            || {
                if !foreground.as_bool() {
                    candwin::hide();
                }
            },
            (),
        );
        Ok(())
    }

    fn OnTestKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        Ok(guard(|| self.on_test_key_down(pic.cloned(), wparam.0 as u32, lparam.0), false).into())
    }

    fn OnTestKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(guard(|| self.on_test_key_up(pic.cloned(), wparam.0 as u32), false).into())
    }

    fn OnKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        Ok(guard(|| self.on_key_down(pic.cloned(), wparam.0 as u32, lparam.0), false).into())
    }

    /// 有的程序不先问 OnTestKeyUp 就直接调这里：照样只吃按下被用掉的键（以前这里一律吃掉）。
    fn OnKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(guard(|| self.on_key_up(pic.cloned(), wparam.0 as u32), false).into())
    }

    /// 切换照做，但永远告诉系统"这个键没用掉"：Shift 的松开要交给程序，
    /// 不然游戏、网页会以为 Shift 一直按着（一直冲刺、一直多选）。
    fn OnPreservedKey(&self, pic: Ref<ITfContext>, rguid: *const GUID) -> Result<BOOL> {
        let ours = !rguid.is_null() && unsafe { *rguid == PRESERVE_SHIFT || *rguid == PRESERVE_SHIFT2 };
        if ours {
            guard(|| self.preserved_shift(pic.cloned()), ());
        }
        Ok(false.into())
    }
}

impl ITfThreadMgrEventSink_Impl for TextService_Impl {
    fn OnInitDocumentMgr(&self, _pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }

    fn OnUninitDocumentMgr(&self, _pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }

    fn OnSetFocus(&self, _focus: Ref<ITfDocumentMgr>, _prev: Ref<ITfDocumentMgr>) -> Result<()> {
        guard(|| self.focus_changed(), ());
        Ok(())
    }

    fn OnPushContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }

    fn OnPopContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }
}

impl ITfCompositionSink_Impl for TextService_Impl {
    /// 程序把我们的输入打断了（比如用鼠标点了别处）：字留在原处，清空状态。
    fn OnCompositionTerminated(&self, _ecwrite: u32, _pcomposition: Ref<ITfComposition>) -> Result<()> {
        guard(
            || {
                self.set_composition(None, None);
                candwin::hide();
                self.reset_engine();
            },
            (),
        );
        Ok(())
    }
}
