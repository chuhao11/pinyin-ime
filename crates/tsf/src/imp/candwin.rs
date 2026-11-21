//! 候选窗：只在打字时出现，贴着光标；不抢焦点、不进任务栏和 Alt+Tab。
//!
//! 样子：圆角、淡阴影；浅色/深色跟随 Windows 设置；高亮的候选是一个朱红色圆角块（和图标的印章一个颜色）。
//! 第一行小字是正在打的拼音；矩阵模式只给高亮那一行标数字。
//! 用 Direct2D + DirectWrite 画，高分屏下清晰；每个线程一个窗口。

use std::cell::RefCell;

use pinyin_types::{Mode, View};
use windows::core::{w, Result, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, IsWindowVisible, KillTimer, RegisterClassExW, SetTimer,
    SetWindowPos, ShowWindow, CS_DROPSHADOW, HWND_TOPMOST, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_PAINT, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::dll_instance;

const FONT: &str = "Microsoft YaHei UI";
const CAND_PT: f32 = 17.0;
const SMALL_PT: f32 = 12.5;
const PAD: f32 = 10.0;
const GAP: f32 = 6.0;
const IPAD_X: f32 = 7.0;
const IPAD_Y: f32 = 4.0;
const LABEL_GAP: f32 = 4.0;
const RADIUS: f32 = 6.0;
const GRID_COLS: usize = 5;
/// 按了一下 ` 的词用的彩虹色：红 橙 黄 绿 蓝 靛 紫
const RAINBOW: [(u8, u8, u8); 7] = [
    (0xFF, 0x5A, 0x4E),
    (0xFF, 0xA0, 0x3A),
    (0xFF, 0xD6, 0x3A),
    (0x5E, 0xD1, 0x6A),
    (0x4A, 0xA8, 0xFF),
    (0x7A, 0x78, 0xF0),
    (0xC7, 0x7D, 0xF0),
];
const FLASH_TIMER: usize = 1;
const FLASH_MS: u32 = 700;

#[derive(Clone, Copy)]
struct Theme {
    bg: D2D1_COLOR_F,
    text: D2D1_COLOR_F,
    dim: D2D1_COLOR_F,
    accent: D2D1_COLOR_F,
    on_accent: D2D1_COLOR_F,
}

fn rgb(r: u8, g: u8, b: u8) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
}

fn theme(dark: bool) -> Theme {
    if dark {
        Theme {
            bg: rgb(0x1F, 0x1F, 0x1F),
            text: rgb(0xEE, 0xEE, 0xEE),
            dim: rgb(0x8E, 0x8E, 0x8E),
            accent: rgb(0xD2, 0x3F, 0x31),
            on_accent: rgb(0xFF, 0xFF, 0xFF),
        }
    } else {
        Theme {
            bg: rgb(0xFF, 0xFF, 0xFF),
            text: rgb(0x1A, 0x1A, 0x1A),
            dim: rgb(0x8A, 0x8A, 0x8A),
            accent: rgb(0xC8, 0x30, 0x2A),
            on_accent: rgb(0xFF, 0xFF, 0xFF),
        }
    }
}

/// Windows 是不是深色模式。
fn system_dark() -> bool {
    let mut v: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok() && v == 0
}

/// 排好版的一个候选。
struct Cell {
    label: Option<IDWriteTextLayout>,
    text: IDWriteTextLayout,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    lw: f32,
    highlight: bool,
}

struct Cand {
    hwnd: HWND,
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    cand_fmt: IDWriteTextFormat,
    small_fmt: IDWriteTextFormat,
    target: Option<ID2D1HwndRenderTarget>,
    view: View,
    dark: bool,
    /// 窗口内容大小（DIP）
    size: (f32, f32),
    pre: Option<IDWriteTextLayout>,
    cells: Vec<Cell>,
    /// 只显示一个"中"/"英"，过一会儿自己消失
    flash: Option<String>,
    caret: Option<RECT>,
}

thread_local! {
    static CAND: RefCell<Option<Cand>> = const { RefCell::new(None) };
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    super::guard(
        || unsafe {
            match msg {
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(hwnd, &mut ps);
                    let painted = CAND.with(|c| match c.try_borrow_mut() {
                        Ok(mut c) => {
                            if let Some(c) = c.as_mut() {
                                let _ = c.paint();
                            }
                            true
                        }
                        Err(_) => false,
                    });
                    let _ = EndPaint(hwnd, &ps);
                    if !painted {
                        // 正忙着更新内容：稍后再画，不留空白窗口
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    LRESULT(0)
                }
                // 点候选窗也不会抢走程序的焦点
                WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
                WM_ERASEBKGND => LRESULT(1),
                WM_TIMER if wp.0 == FLASH_TIMER => {
                    let _ = KillTimer(Some(hwnd), FLASH_TIMER);
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        },
        LRESULT(0),
    )
}

fn create() -> Result<Cand> {
    unsafe {
        let class = w!("PinyinIMECandidate");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: dll_instance(),
            lpszClassName: class,
            ..Default::default()
        };
        // 同一个程序里注册过就会失败，没关系
        let _ = RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!(""),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(dll_instance()),
            None,
        )?;
        // Windows 11 的圆角
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const _,
            std::mem::size_of_val(&pref) as u32,
        );
        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let mk = |size: f32, weight| -> Result<IDWriteTextFormat> {
            let f = dw.CreateTextFormat(
                &HSTRING::from(FONT),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("zh-CN"),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(f)
        };
        let cand_fmt = mk(CAND_PT, DWRITE_FONT_WEIGHT_NORMAL)?;
        let small_fmt = mk(SMALL_PT, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        Ok(Cand {
            hwnd,
            d2d,
            dw,
            cand_fmt,
            small_fmt,
            target: None,
            view: View {
                composing: false,
                preedit: String::new(),
                mode: Mode::Row,
                items: Vec::new(),
                highlight: 0,
                page: 0,
                pages: 0,
                total: 0,
                corrected: Vec::new(),
                english_mode: false,
                armed: false,
            },
            dark: false,
            size: (10.0, 10.0),
            pre: None,
            cells: Vec::new(),
            flash: None,
            caret: None,
        })
    }
}

impl Cand {
    fn layout(&self, text: &str, small: bool) -> Result<(IDWriteTextLayout, f32, f32)> {
        let w: Vec<u16> = text.encode_utf16().collect();
        let fmt = if small { &self.small_fmt } else { &self.cand_fmt };
        unsafe {
            let l = self.dw.CreateTextLayout(&w, fmt, 4000.0, 400.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m)?;
            Ok((l, m.widthIncludingTrailingWhitespace, m.height))
        }
    }

    /// 根据 view 排版，算出窗口大小（DIP）。
    fn relayout(&mut self) -> Result<()> {
        if let Some(f) = self.flash.clone() {
            self.pre = None;
            let (t, tw, th) = self.layout(&f, false)?;
            let (w, h) = (tw + 2.0 * IPAD_X + 4.0, th + 2.0 * IPAD_Y);
            self.cells = vec![Cell { label: None, text: t, x: 4.0, y: 4.0, w, h, lw: 0.0, highlight: true }];
            self.size = (w + 8.0, h + 8.0);
            return Ok(());
        }
        let v = self.view.clone();
        let header = match (v.armed, v.mode) {
            // 按了一下 `：提示再按一次就删
            (true, _) => {
                format!("再按一次 ` 删除「{}」，按别的键取消", v.items.get(v.highlight).cloned().unwrap_or_default())
            }
            (false, Mode::Row) => v.preedit.clone(),
            (false, Mode::Grid) => format!("{}    第 {}/{} 批", v.preedit, v.page + 1, v.pages.max(1)),
        };
        let (pre, pw, ph) = self.layout(&header, true)?;
        self.pre = Some(pre);
        let mut cells = Vec::new();
        let top = PAD + ph + 6.0;
        let hl_row = v.highlight / GRID_COLS;
        // 先量每个候选
        let mut measured = Vec::new();
        for (i, item) in v.items.iter().enumerate() {
            let mark = if v.corrected.get(i).copied().unwrap_or(false) { "~" } else { "" };
            let (t, tw, th) = self.layout(&format!("{item}{mark}"), false)?;
            let show_label = match v.mode {
                Mode::Row => true,
                Mode::Grid => i / GRID_COLS == hl_row,
            };
            let label_n = match v.mode {
                Mode::Row => i + 1,
                Mode::Grid => i % GRID_COLS + 1,
            };
            let (l, lw) = if show_label {
                let (l, lw, _) = self.layout(&label_n.to_string(), true)?;
                (Some(l), lw + LABEL_GAP)
            } else {
                (None, 0.0)
            };
            measured.push((l, lw, t, tw, th, i == v.highlight));
        }
        let mut width: f32 = pw;
        let height;
        match v.mode {
            Mode::Row => {
                let mut x = PAD;
                let mut h: f32 = 0.0;
                for (l, lw, t, tw, th, hl) in measured {
                    let w = lw + tw + 2.0 * IPAD_X;
                    cells.push(Cell { label: l, text: t, x, y: top, w, h: th + 2.0 * IPAD_Y, lw, highlight: hl });
                    x += w + GAP;
                    h = h.max(th + 2.0 * IPAD_Y);
                }
                width = width.max(x - GAP - PAD);
                height = top + h;
            }
            Mode::Grid => {
                // 等宽的列：数字的位置留出来，各行对齐
                let label_w = self.layout("5", true)?.1 + LABEL_GAP;
                let col_w = measured.iter().map(|m| m.3).fold(0.0f32, f32::max) + label_w + 2.0 * IPAD_X;
                let row_h = measured.iter().map(|m| m.4).fold(0.0f32, f32::max) + 2.0 * IPAD_Y;
                for (i, (l, _lw, t, _tw, _th, hl)) in measured.into_iter().enumerate() {
                    let (r, c) = (i / GRID_COLS, i % GRID_COLS);
                    let x = PAD + c as f32 * (col_w + GAP);
                    let y = top + r as f32 * (row_h + 2.0);
                    cells.push(Cell { label: l, text: t, x, y, w: col_w, h: row_h, lw: label_w, highlight: hl });
                }
                let rows = cells.len().div_ceil(GRID_COLS).max(1);
                let cols = cells.len().clamp(1, GRID_COLS);
                width = width.max(cols as f32 * (col_w + GAP) - GAP);
                height = top + rows as f32 * (row_h + 2.0) - 2.0;
            }
        }
        self.cells = cells;
        self.size = (width + 2.0 * PAD, height + PAD);
        Ok(())
    }

    fn paint(&mut self) -> Result<()> {
        unsafe {
            let dpi = GetDpiForWindow(self.hwnd).max(96) as f32;
            let px = D2D_SIZE_U {
                width: (self.size.0 * dpi / 96.0).ceil() as u32,
                height: (self.size.1 * dpi / 96.0).ceil() as u32,
            };
            if self.target.is_none() {
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    // 软件渲染：不往每个打字的程序里加载显卡驱动（省内存），这么小的窗口画起来不到 1 毫秒
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                    },
                    dpiX: 0.0,
                    dpiY: 0.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let hp = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd: self.hwnd,
                    pixelSize: px,
                    presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                };
                self.target = Some(self.d2d.CreateHwndRenderTarget(&props, &hp)?);
            }
            let rt = self.target.clone().unwrap();
            let _ = rt.Resize(&px);
            rt.SetDpi(dpi, dpi);
            let th = theme(self.dark);
            let brush = |c: D2D1_COLOR_F| -> Result<ID2D1SolidColorBrush> { rt.CreateSolidColorBrush(&c, None) };
            let (text_b, dim_b, acc_b, on_b) =
                (brush(th.text)?, brush(th.dim)?, brush(th.accent)?, brush(th.on_accent)?);
            rt.BeginDraw();
            rt.Clear(Some(&th.bg));
            if let Some(pre) = &self.pre {
                rt.DrawTextLayout(
                    windows_numerics::Vector2 { X: PAD + 2.0, Y: PAD },
                    pre,
                    &dim_b,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            let armed = self.view.armed && self.flash.is_none();
            let dark_b = brush(rgb(0x14, 0x14, 0x14))?;
            for c in &self.cells {
                if c.highlight {
                    let rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F { left: c.x, top: c.y, right: c.x + c.w, bottom: c.y + c.h },
                        radiusX: RADIUS,
                        radiusY: RADIUS,
                    };
                    if armed {
                        // 按了一下 `：这个词变成彩虹色，再按一下就删
                        let stops: Vec<D2D1_GRADIENT_STOP> = RAINBOW
                            .iter()
                            .enumerate()
                            .map(|(i, &(r, g, b))| D2D1_GRADIENT_STOP {
                                position: i as f32 / (RAINBOW.len() - 1) as f32,
                                color: rgb(r, g, b),
                            })
                            .collect();
                        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
                        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                            startPoint: windows_numerics::Vector2 { X: c.x, Y: c.y },
                            endPoint: windows_numerics::Vector2 { X: c.x + c.w, Y: c.y + c.h },
                        };
                        let rainbow = rt.CreateLinearGradientBrush(&props, None, &coll)?;
                        rt.FillRoundedRectangle(&rr, &rainbow);
                    } else {
                        rt.FillRoundedRectangle(&rr, &acc_b);
                    }
                }
                let (lb, tb) = match (c.highlight, armed) {
                    // 彩虹底上用深色字，黄色、绿色那段也看得清
                    (true, true) => (&dark_b, &dark_b),
                    (true, false) => (&on_b, &on_b),
                    _ => (&dim_b, &text_b),
                };
                if let Some(l) = &c.label {
                    // 数字比候选字小，和字的中线对齐
                    rt.DrawTextLayout(
                        windows_numerics::Vector2 { X: c.x + IPAD_X, Y: c.y + IPAD_Y + 3.0 },
                        l,
                        lb,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
                rt.DrawTextLayout(
                    windows_numerics::Vector2 { X: c.x + IPAD_X + c.lw, Y: c.y + IPAD_Y },
                    &c.text,
                    tb,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            if rt.EndDraw(None, None).is_err() {
                // 显卡设备丢了（比如切换了显卡）：下次重建
                self.target = None;
            }
        }
        Ok(())
    }

    fn present(&mut self, caret: RECT) {
        self.dark = system_dark();
        self.caret = Some(caret);
        if self.relayout().is_err() {
            return;
        }
        let before = unsafe { GetDpiForWindow(self.hwnd) };
        self.place(caret);
        // 换到了缩放比例不同的显示器上：按新的比例再排一次
        if unsafe { GetDpiForWindow(self.hwnd) } != before {
            self.place(caret);
        }
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// 放到光标下面；下面放不下就放到上面；不跑出屏幕。
    fn place(&self, caret: RECT) {
        unsafe {
            let dpi = GetDpiForWindow(self.hwnd).max(96) as f32;
            let w = (self.size.0 * dpi / 96.0).ceil() as i32;
            let h = (self.size.1 * dpi / 96.0).ceil() as i32;
            let mon = MonitorFromRect(&caret, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let work = if GetMonitorInfoW(mon, &mut mi).as_bool() {
                mi.rcWork
            } else {
                RECT { left: 0, top: 0, right: 1920, bottom: 1080 }
            };
            let gap = (4.0 * dpi / 96.0) as i32;
            let mut x = caret.left;
            let mut y = caret.bottom + gap;
            if y + h > work.bottom {
                y = caret.top - gap - h;
            }
            if x + w > work.right {
                x = work.right - w;
            }
            x = x.max(work.left);
            y = y.max(work.top);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }
}

/// 显示（或更新）候选窗。
pub fn show(caret: RECT, view: &View) {
    with_window(|cand| {
        cand.view = view.clone();
        cand.flash = None;
        unsafe {
            let _ = KillTimer(Some(cand.hwnd), FLASH_TIMER);
        }
        cand.present(caret);
    });
}

/// 切换中英后在光标旁闪一下"中"/"英"，0.7 秒后自己消失。
pub fn flash(caret: RECT, text: &str) {
    with_window(|cand| {
        cand.flash = Some(text.to_string());
        cand.present(caret);
        unsafe {
            SetTimer(Some(cand.hwnd), FLASH_TIMER, FLASH_MS, None);
        }
    });
}

/// 程序排好版后知道了字的准确位置：把正在显示的候选窗挪过去。
pub fn move_to(caret: RECT) {
    CAND.with(|c| {
        let Ok(mut slot) = c.try_borrow_mut() else { return };
        let Some(cand) = slot.as_mut() else { return };
        if cand.flash.is_none() && unsafe { IsWindowVisible(cand.hwnd) }.as_bool() {
            cand.caret = Some(caret);
            cand.place(caret);
        }
    });
}

/// 上一次候选窗贴着的位置。
pub fn last_rect() -> Option<RECT> {
    CAND.with(|c| c.try_borrow().ok().and_then(|s| s.as_ref().and_then(|c| c.caret)))
}

fn with_window(f: impl FnOnce(&mut Cand)) {
    CAND.with(|c| {
        let Ok(mut slot) = c.try_borrow_mut() else { return };
        if slot.is_none() {
            match create() {
                Ok(x) => *slot = Some(x),
                Err(_) => return,
            }
        }
        if let Some(cand) = slot.as_mut() {
            f(cand);
        }
    });
}

pub fn hide() {
    CAND.with(|c| {
        if let Ok(slot) = c.try_borrow() {
            if let Some(cand) = slot.as_ref() {
                unsafe {
                    let _ = KillTimer(Some(cand.hwnd), FLASH_TIMER);
                    let _ = ShowWindow(cand.hwnd, SW_HIDE);
                }
            }
        }
    });
}

pub fn destroy() {
    CAND.with(|c| {
        if let Ok(mut slot) = c.try_borrow_mut() {
            if let Some(cand) = slot.take() {
                unsafe {
                    let _ = DestroyWindow(cand.hwnd);
                }
            }
        }
    });
}
