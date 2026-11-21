//! 输入法对象：每个程序的每个界面线程一个。接收按键、问引擎、改文档里的字。
//!
//! 和别的程序和平相处的规矩（详见 docs/方案.md「和别的程序不冲突」）：
//! - 没在打字就什么都不显示、不拦任何键；Ctrl/Alt/Win 组合键、F1–F12、修饰键一律放过
//! - 下面这些情况所有键原样交给程序：没有输入框、只读、密码框、数字框、程序关掉了输入法、
//!   引擎没运行或卡住（超过 0.3 秒）
//! - 全屏游戏里默认不打字（按键全交给游戏）；单按一下 Shift 才临时打中文，回车或 Esc 发出去后自动恢复
//! - 任何出错都兜住，绝不让程序崩溃

use std::cell::RefCell;

use pinyin_ipc::{Reply, Request};
use pinyin_types::{Key, Mods};
use windows::core::{implement, ComObject, IUnknownImpl, Interface, Ref, Result, BOOL, GUID};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_BUSY, QUNS_RUNNING_D3D_FULL_SCREEN};
use windows::Win32::UI::TextServices::{
    ITfCompartmentMgr, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfDocumentMgr,
    ITfEditSession, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource, ITfTextInputProcessorEx,
    ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl, ITfThreadMgr, ITfThreadMgrEventSink,
    ITfThreadMgrEventSink_Impl, GUID_COMPARTMENT_EMPTYCONTEXT, GUID_COMPARTMENT_KEYBOARD_DISABLED,
    GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, TF_ES_ASYNCDONTCARE, TF_ES_READ, TF_ES_READWRITE, TF_ES_SYNC, TS_SD_READONLY,
    TS_SS_TRANSITORY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO};

use super::client::EngineClient;
use super::edit::{Action, EditSession};
use super::keys::{self, ShiftTracker};
use super::{candwin, guard};

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
    /// 按下时被输入法吃掉的键：它们松开时也吃掉，程序不会收到只有"松开"的键
    eaten: Vec<u32>,
    /// 全屏游戏里按 Shift 临时打开了中文
    game_chinese: bool,
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
        st.composition = c;
        st.comp_ctx = ctx;
    }

    fn composing(&self) -> bool {
        self.st.borrow().composition.is_some()
    }

    fn call(&self, req: &Request) -> Option<Reply> {
        self.st.borrow_mut().client.call(req)
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

impl TextService_Impl {
    fn edit(&self, ctx: &ITfContext, action: Action, sync: bool) -> bool {
        let tid = self.tid();
        let es = ComObject::new(EditSession::new(ctx.clone(), self.to_object(), action));
        let i: ITfEditSession = es.to_interface();
        let check = matches!(es.action, Action::CheckScope);
        let flags = match (check, sync) {
            (true, _) => TF_ES_SYNC | TF_ES_READ,
            (false, true) => TF_ES_SYNC | TF_ES_READWRITE,
            (false, false) => TF_ES_ASYNCDONTCARE | TF_ES_READWRITE,
        };
        let mut ok = unsafe { ctx.RequestEditSession(tid, &i, flags) }.is_ok_and(|hr| hr.is_ok());
        if !ok && sync && !check {
            // 程序这会儿不让立刻改：排队稍后改（字不会丢）
            ok = unsafe { ctx.RequestEditSession(tid, &i, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE) }
                .is_ok_and(|hr| hr.is_ok());
        }
        ok && es.result.get()
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

    /// 这个输入框里输入法该不该工作。
    fn blocked(&self, ctx: &ITfContext) -> bool {
        unsafe {
            // 程序或用户关掉了输入法（Ctrl+空格、ImmSetOpenStatus）
            let tm = self.st.borrow().tm.clone();
            if let Some(tm) = tm {
                if let Ok(m) = tm.cast::<ITfCompartmentMgr>() {
                    if compartment(&m, &GUID_COMPARTMENT_KEYBOARD_OPENCLOSE) == Some(0) {
                        return true;
                    }
                }
            }
            // 程序禁用了输入法（很多游戏、密码框这样做）
            if let Ok(m) = ctx.cast::<ITfCompartmentMgr>() {
                for g in [GUID_COMPARTMENT_KEYBOARD_DISABLED, GUID_COMPARTMENT_EMPTYCONTEXT] {
                    if compartment(&m, &g).is_some_and(|v| v != 0) {
                        return true;
                    }
                }
            }
            if let Ok(s) = ctx.GetStatus() {
                if s.dwDynamicFlags & TS_SD_READONLY != 0 {
                    return true;
                }
            }
        }
        // 密码框、数字框
        self.edit(ctx, Action::CheckScope, true)
    }

    /// 全屏游戏：全屏，又不是真正的文字输入框（没有光标的老式窗口）。
    fn in_game(&self, ctx: &ITfContext) -> bool {
        let fullscreen = unsafe { SHQueryUserNotificationState() }
            .is_ok_and(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_BUSY);
        if !fullscreen {
            return false;
        }
        let transitory = unsafe { ctx.GetStatus() }.is_ok_and(|s| s.dwStaticFlags & TS_SS_TRANSITORY != 0);
        transitory && !has_system_caret()
    }

    fn test_key_down(&self, ctx: Option<ITfContext>, vk: u32, lp: isize) -> bool {
        if keys::is_shift(vk) {
            self.st.borrow_mut().shift.shift_down();
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
                match self.call(&Request::Key { key, mods }) {
                    Some(Reply::KeyResult { commit, view, .. }) => {
                        self.edit(&ctx, Action::Update { commit, view, flash: None }, true);
                    }
                    _ => self.end_composition(),
                }
                return false;
            }
        } else if keys::is_numpad(vk) {
            self.pass_through(key, mods);
            return false;
        }
        match self.call(&Request::TestKey { key, mods }) {
            Some(Reply::Bool(true)) => {
                if composing {
                    return true;
                }
                // 要开始打一段新的拼音：先看这里该不该打
                if self.blocked(&ctx) {
                    return false;
                }
                if self.in_game(&ctx) && !self.st.borrow().game_chinese {
                    return false;
                }
                true
            }
            Some(Reply::Bool(false)) => {
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
        if let Some(Reply::KeyResult { consumed: true, .. }) = self.call(&Request::Key { key, mods }) {
            // 两边不一致（不该发生）：丢掉引擎里的状态
            self.notify(&Request::Reset);
        }
        if matches!(key, Key::Enter | Key::Escape) {
            // 游戏里聊天发出去了 / 关掉了：恢复成按键全交给游戏
            self.st.borrow_mut().game_chinese = false;
        }
    }

    fn key_down(&self, ctx: Option<ITfContext>, vk: u32, lp: isize) -> bool {
        let Some(ctx) = ctx else { return false };
        let Some(key) = keys::translate(vk, lp) else { return false };
        let mods = keys::mods();
        match self.call(&Request::Key { key, mods }) {
            Some(Reply::KeyResult { consumed, commit, view }) => {
                if consumed || commit.is_some() || self.composing() {
                    self.edit(&ctx, Action::Update { commit, view, flash: None }, false);
                }
                if consumed {
                    let mut st = self.st.borrow_mut();
                    if st.eaten.len() >= 16 {
                        st.eaten.clear();
                    }
                    st.eaten.push(vk);
                }
                consumed
            }
            _ => {
                if self.composing() {
                    self.end_composition();
                }
                false
            }
        }
    }

    fn test_key_up(&self, ctx: Option<ITfContext>, vk: u32) -> bool {
        if keys::is_shift(vk) {
            let tap = self.st.borrow_mut().shift.shift_up();
            if tap {
                self.shift_tap(ctx);
            }
            // Shift 松开永远交给程序
            return false;
        }
        let mut st = self.st.borrow_mut();
        if let Some(i) = st.eaten.iter().position(|&k| k == vk) {
            st.eaten.swap_remove(i);
            return true;
        }
        false
    }

    /// 单按一下 Shift：中文 ↔ 纯英文；在光标旁边闪一下"中"或"英"。
    fn shift_tap(&self, ctx: Option<ITfContext>) {
        let composing = self.composing();
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
        match self.call(&Request::Key { key: Key::ShiftTap, mods: Mods::default() }) {
            Some(Reply::KeyResult { commit, view, .. }) => {
                let flash = if view.english_mode { "英" } else { "中" };
                if let Some(ctx) = &ctx {
                    self.edit(ctx, Action::Update { commit, view, flash: Some(flash) }, false);
                }
            }
            _ => {
                if composing {
                    self.end_composition();
                }
            }
        }
    }

    fn focus_changed(&self) {
        self.end_composition();
        self.notify(&Request::Reset);
        self.notify(&Request::ClearContext);
        let mut st = self.st.borrow_mut();
        st.game_chinese = false;
        st.eaten.clear();
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
                let (tm, tid, cookie, key_sink) = {
                    let mut st = self.st.borrow_mut();
                    (st.tm.take(), st.tid, st.tm_cookie.take(), std::mem::take(&mut st.key_sink))
                };
                if let Some(tm) = tm {
                    unsafe {
                        if key_sink {
                            if let Ok(km) = tm.cast::<ITfKeystrokeMgr>() {
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
                self.notify(&Request::Reset);
                let mut st = self.st.borrow_mut();
                st.client.disconnect();
                st.eaten.clear();
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
                    let src: ITfSource = tm.cast()?;
                    let cookie =
                        src.AdviseSink(&ITfThreadMgrEventSink::IID, &self.to_interface::<ITfThreadMgrEventSink>())?;
                    self.st.borrow_mut().tm_cookie = Some(cookie);
                    // 打开输入法（和微软的示例一样）；程序或 Ctrl+空格 可以再关掉
                    if let Ok(m) = tm.cast::<ITfCompartmentMgr>() {
                        if let Ok(c) = m.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE) {
                            let _ = c.SetValue(tid, &VARIANT::from(1i32));
                        }
                    }
                }
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
        Ok(guard(|| self.test_key_down(pic.cloned(), wparam.0 as u32, lparam.0), false).into())
    }

    fn OnTestKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(guard(|| self.test_key_up(pic.cloned(), wparam.0 as u32), false).into())
    }

    fn OnKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        Ok(guard(|| self.key_down(pic.cloned(), wparam.0 as u32, lparam.0), false).into())
    }

    fn OnKeyUp(&self, _pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        // 只有 OnTestKeyUp 说要吃的键才会到这里（它已经从列表里删掉了）
        let _ = wparam;
        Ok(true.into())
    }

    fn OnPreservedKey(&self, _pic: Ref<ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
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
                self.notify(&Request::Reset);
            },
            (),
        );
        Ok(())
    }
}
