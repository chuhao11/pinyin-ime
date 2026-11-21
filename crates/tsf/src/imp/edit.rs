//! 编辑会话：TSF 只允许在"编辑会话"里读写文档里的字。
//!
//! - 上屏：把字写进"正在打的那段"，光标放到后面，结束这一段
//! - 正在打：那段里显示拼音，光标放在后面，候选窗贴着这段字显示
//! - 取消：把那段删掉

use std::cell::Cell;
use std::mem::ManuallyDrop;

use pinyin_types::View;
use windows::core::{implement, ComObject, IUnknown, Interface, Result};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::MapWindowPoints;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfContext, ITfContextComposition, ITfEditSession, ITfEditSession_Impl,
    ITfInputScope, ITfInsertAtSelection, ITfRange, InputScope, GUID_PROP_INPUTSCOPE, IS_ALPHANUMERIC_PIN,
    IS_ALPHANUMERIC_PIN_SET, IS_DIGITS, IS_NUMBER, IS_NUMERIC_PASSWORD, IS_NUMERIC_PIN, IS_PASSWORD, TF_AE_NONE,
    TF_ANCHOR_END, TF_DEFAULT_SELECTION, TF_ES_ASYNC, TF_ES_READ, TF_IAS_NOQUERY, TF_IAS_QUERYONLY, TF_SELECTION,
    TF_SELECTIONSTYLE,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetGUIThreadInfo, GUITHREADINFO};

use super::candwin;
use super::guard;
use super::service::TextService;

pub enum Action {
    /// 先上屏 `commit`，再按 `view` 显示正在打的拼音和候选窗；`flash` 是切换中英后闪一下的字
    Update { commit: Option<String>, view: View, flash: Option<&'static str> },
    /// 结束正在打的这段（字留在文档里）
    End(ITfComposition),
    /// 在光标旁边闪一下"中"/"英"
    Flash(&'static str),
    /// 只读：光标所在的是不是密码框、数字框（结果放在 `result`）
    CheckScope,
    /// 只读：程序排好版以后，把候选窗挪到正确的位置
    Reposition,
}

#[implement(ITfEditSession)]
pub struct EditSession {
    ctx: ITfContext,
    svc: ComObject<TextService>,
    pub action: Action,
    pub result: Cell<bool>,
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// 密码、PIN、纯数字的输入框：输入法不工作。
const BLOCKED_SCOPES: [InputScope; 7] = [
    IS_PASSWORD,
    IS_NUMERIC_PASSWORD,
    IS_NUMERIC_PIN,
    IS_ALPHANUMERIC_PIN,
    IS_ALPHANUMERIC_PIN_SET,
    IS_DIGITS,
    IS_NUMBER,
];

/// 程序没给出文字位置时的退路：系统光标的位置，再不行就是鼠标位置。
fn fallback_rect() -> RECT {
    unsafe {
        let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(GetCurrentThreadId(), &mut gti).is_ok() && !gti.hwndCaret.is_invalid() {
            let r = gti.rcCaret;
            let mut pts = [POINT { x: r.left, y: r.top }, POINT { x: r.right, y: r.bottom }];
            MapWindowPoints(Some(gti.hwndCaret), None, &mut pts);
            return RECT { left: pts[0].x, top: pts[0].y, right: pts[1].x, bottom: pts[1].y };
        }
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        RECT { left: p.x, top: p.y, right: p.x + 1, bottom: p.y + 20 }
    }
}

impl EditSession {
    pub fn new(ctx: ITfContext, svc: ComObject<TextService>, action: Action) -> Self {
        EditSession { ctx, svc, action, result: Cell::new(false) }
    }

    fn start_composition(&self, ec: u32) -> Result<ITfComposition> {
        unsafe {
            let ias: ITfInsertAtSelection = self.ctx.cast()?;
            let range = ias.InsertTextAtSelection(ec, TF_IAS_QUERYONLY, &[])?;
            let cc: ITfContextComposition = self.ctx.cast()?;
            let sink: ITfCompositionSink = self.svc.to_interface();
            let comp = cc.StartComposition(ec, &range, &sink)?;
            self.svc.set_composition(Some(comp.clone()), Some(self.ctx.clone()));
            Ok(comp)
        }
    }

    fn caret_after(&self, ec: u32, range: &ITfRange) -> Result<()> {
        unsafe {
            let r = range.Clone()?;
            r.Collapse(ec, TF_ANCHOR_END)?;
            let mut sel = TF_SELECTION {
                range: ManuallyDrop::new(Some(r)),
                style: TF_SELECTIONSTYLE { ase: TF_AE_NONE, fInterimChar: false.into() },
            };
            let res = self.ctx.SetSelection(ec, std::slice::from_ref(&sel));
            ManuallyDrop::drop(&mut sel.range);
            res
        }
    }

    /// 文字在屏幕上的位置。程序还没排好版时返回 None。
    fn text_rect(&self, ec: u32, range: &ITfRange) -> Option<RECT> {
        unsafe {
            let view = self.ctx.GetActiveView().ok()?;
            let mut rc = RECT::default();
            let mut clipped = Default::default();
            view.GetTextExt(ec, range, &mut rc, &mut clipped).ok()?;
            if rc.left == 0 && rc.top == 0 && rc.right == 0 && rc.bottom == 0 {
                return None;
            }
            Some(rc)
        }
    }

    fn selection_range(&self, ec: u32) -> Option<ITfRange> {
        unsafe {
            let mut sel = [TF_SELECTION::default()];
            let mut n = 0u32;
            self.ctx.GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut n).ok()?;
            if n == 0 {
                return None;
            }
            ManuallyDrop::take(&mut sel[0].range)
        }
    }

    fn later_reposition(&self) {
        let tid = self.svc.tid();
        let es = ComObject::new(EditSession::new(self.ctx.clone(), self.svc.clone(), Action::Reposition));
        let i: ITfEditSession = es.to_interface();
        unsafe {
            let _ = self.ctx.RequestEditSession(tid, &i, TF_ES_ASYNC | TF_ES_READ);
        }
    }

    fn commit(&self, ec: u32, text: &str) -> Result<()> {
        let comp = match self.svc.composition() {
            Some(c) => Some(c),
            None => self.start_composition(ec).ok(),
        };
        unsafe {
            match comp {
                Some(comp) => {
                    let range = comp.GetRange()?;
                    range.SetText(ec, 0, &utf16(text))?;
                    let _ = self.caret_after(ec, &range);
                    let _ = comp.EndComposition(ec);
                    self.svc.set_composition(None, None);
                }
                None => {
                    // 程序不让开始一段输入：直接插到光标处
                    let ias: ITfInsertAtSelection = self.ctx.cast()?;
                    ias.InsertTextAtSelection(ec, TF_IAS_NOQUERY, &utf16(text))?;
                }
            }
        }
        Ok(())
    }

    fn update(&self, ec: u32, commit: &Option<String>, view: &View, flash: Option<&str>) -> Result<()> {
        if let Some(t) = commit {
            self.commit(ec, t)?;
        }
        unsafe {
            if view.composing {
                let comp = match self.svc.composition() {
                    Some(c) => c,
                    None => self.start_composition(ec)?,
                };
                let range = comp.GetRange()?;
                range.SetText(ec, 0, &utf16(&view.preedit))?;
                let _ = self.caret_after(ec, &range);
                match self.text_rect(ec, &range) {
                    Some(rc) => candwin::show(rc, view),
                    None => {
                        candwin::show(candwin::last_rect().unwrap_or_else(fallback_rect), view);
                        self.later_reposition();
                    }
                }
            } else {
                if let Some(comp) = self.svc.composition() {
                    // 取消（Esc、退格删光）：把那段拼音删掉
                    if let Ok(range) = comp.GetRange() {
                        let _ = range.SetText(ec, 0, &[]);
                    }
                    let _ = comp.EndComposition(ec);
                    self.svc.set_composition(None, None);
                }
                candwin::hide();
                if let Some(f) = flash {
                    self.flash(ec, f);
                }
            }
        }
        Ok(())
    }

    fn flash(&self, ec: u32, text: &str) {
        let rc = self.selection_range(ec).and_then(|r| self.text_rect(ec, &r)).unwrap_or_else(fallback_rect);
        candwin::flash(rc, text);
    }

    /// 结束那一段：字留在文档里，光标放到后面。状态在请求时就已经清掉了
    /// （这时可能已经开始了新的一段，不能再动它）。
    fn end(&self, ec: u32, comp: &ITfComposition) {
        unsafe {
            if let Ok(range) = comp.GetRange() {
                let _ = self.caret_after(ec, &range);
            }
            let _ = comp.EndComposition(ec);
        }
    }

    /// 光标处的输入框是不是密码框、数字框。
    fn blocked_scope(&self, ec: u32) -> bool {
        unsafe {
            let Some(range) = self.selection_range(ec) else { return false };
            let Ok(prop) = self.ctx.GetAppProperty(&GUID_PROP_INPUTSCOPE) else { return false };
            let Ok(v) = prop.GetValue(ec, &range) else { return false };
            let Ok(unk) = IUnknown::try_from(&v) else { return false };
            let Ok(is) = unk.cast::<ITfInputScope>() else { return false };
            let mut p: *mut InputScope = std::ptr::null_mut();
            let mut n = 0u32;
            if is.GetInputScopes(&mut p, &mut n).is_err() || p.is_null() {
                return false;
            }
            let scopes = std::slice::from_raw_parts(p, n as usize);
            let blocked = scopes.iter().any(|s| BLOCKED_SCOPES.contains(s));
            CoTaskMemFree(Some(p as *const _));
            blocked
        }
    }

    fn run(&self, ec: u32) -> Result<()> {
        match &self.action {
            Action::Update { commit, view, flash } => self.update(ec, commit, view, *flash)?,
            Action::End(comp) => self.end(ec, comp),
            Action::Flash(t) => self.flash(ec, t),
            Action::CheckScope => self.result.set(self.blocked_scope(ec)),
            Action::Reposition => {
                if let Some(comp) = self.svc.composition() {
                    if let Some(rc) = unsafe { comp.GetRange() }.ok().and_then(|r| self.text_rect(ec, &r)) {
                        candwin::move_to(rc);
                    }
                }
            }
        }
        self.result.set(match &self.action {
            Action::CheckScope => self.result.get(),
            _ => true,
        });
        Ok(())
    }
}

impl ITfEditSession_Impl for EditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        guard(|| self.run(ec), Ok(()))
    }
}
