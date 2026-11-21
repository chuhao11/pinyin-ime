//! 设置页窗口：Direct2D 软件渲染 + DirectWrite；跟随 Windows 的深色 / 浅色；高分屏清晰。
//!
//! ┌─────────────────────────────────────────┐
//! │ [图标]  拼音输入法                        │
//! │         设置                              │
//! │ ┌─────────────────────────────────────┐ │
//! │ │ 文风                    [日常|书面]  │ │
//! │ │ 日常适合聊天，书面适合写文章          │ │
//! │ └─────────────────────────────────────┘ │
//! │ ┌─────────────────────────────────────┐ │
//! │ │ 中文标点                     (  ●)   │ │
//! │ │ 开：，。？！ 全部中文 …               │ │
//! │ └─────────────────────────────────────┘ │
//! │      改动立即生效 · 单按 Shift 切换中英    │
//! └─────────────────────────────────────────┘

use std::cell::RefCell;

use pinyin_ipc::{settings_path, Reply, Request, Settings, PROTO};
use windows::core::{s, w, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, GENERIC_READ, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1RenderTarget, ID2D1SolidColorBrush,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetMessageW, LoadCursorW, LoadImageW,
    PostQuitMessage, RegisterClassExW, SetCursor, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HCURSOR, HICON, HTCLIENT, IDC_ARROW, IDC_HAND, IMAGE_ICON, LR_DEFAULTSIZE,
    LR_LOADFROMFILE, MSG, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE, SW_SHOW, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WM_SETTINGCHANGE, WM_SIZE, WNDCLASSEXW,
    WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
};

const CLASS: PCWSTR = w!("PinyinIMESettings");
const FONT: &str = "Microsoft YaHei UI";

/// 窗口内容大小（DIP）
const W: f32 = 440.0;
const H: f32 = 320.0;

const CARD1: D2D_RECT_F = D2D_RECT_F { left: 16.0, top: 96.0, right: W - 16.0, bottom: 176.0 };
const CARD2: D2D_RECT_F = D2D_RECT_F { left: 16.0, top: 188.0, right: W - 16.0, bottom: 268.0 };
const SEG_W: f32 = 148.0;
const SEG_H: f32 = 34.0;
const SW_W: f32 = 50.0;
const SW_H: f32 = 28.0;

fn seg_rect() -> D2D_RECT_F {
    let cy = (CARD1.top + CARD1.bottom) / 2.0;
    D2D_RECT_F {
        left: CARD1.right - 18.0 - SEG_W,
        top: cy - SEG_H / 2.0,
        right: CARD1.right - 18.0,
        bottom: cy + SEG_H / 2.0,
    }
}

fn switch_rect() -> D2D_RECT_F {
    let cy = (CARD2.top + CARD2.bottom) / 2.0;
    D2D_RECT_F {
        left: CARD2.right - 18.0 - SW_W,
        top: cy - SW_H / 2.0,
        right: CARD2.right - 18.0,
        bottom: cy + SW_H / 2.0,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Daily,
    Formal,
    Punct,
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

fn hit(x: f32, y: f32) -> Option<Hit> {
    let s = seg_rect();
    if inside(&s, x, y) {
        return Some(if x < (s.left + s.right) / 2.0 { Hit::Daily } else { Hit::Formal });
    }
    // 整张"中文标点"卡片都能点
    if inside(&CARD2, x, y) {
        return Some(Hit::Punct);
    }
    None
}

struct Theme {
    bg: D2D1_COLOR_F,
    card: D2D1_COLOR_F,
    text: D2D1_COLOR_F,
    dim: D2D1_COLOR_F,
    accent: D2D1_COLOR_F,
    track: D2D1_COLOR_F,
    knob: D2D1_COLOR_F,
}

fn rgb(c: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((c >> 16) & 0xFF) as f32 / 255.0,
        g: ((c >> 8) & 0xFF) as f32 / 255.0,
        b: (c & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}

fn theme(dark: bool) -> Theme {
    if dark {
        Theme {
            bg: rgb(0x121212),
            card: rgb(0x1D1D1D),
            text: rgb(0xEDEDED),
            dim: rgb(0x8C8C8C),
            accent: rgb(0xD23F31),
            track: rgb(0x3A3A3A),
            knob: rgb(0xFFFFFF),
        }
    } else {
        Theme {
            bg: rgb(0xF4F3F1),
            card: rgb(0xFFFFFF),
            text: rgb(0x1A1A1A),
            dim: rgb(0x7E7E7E),
            accent: rgb(0xC8302A),
            track: rgb(0xD5D3D0),
            knob: rgb(0xFFFFFF),
        }
    }
}

fn system_dark() -> bool {
    let mut v: u32 = 1;
    let mut size = 4u32;
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

fn exe_dir() -> std::path::PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default()
}

/// 告诉引擎马上重新读设置（引擎没在运行也没关系：它启动时会读，平时两秒内也会自己发现）。
fn ping_engine() {
    std::thread::spawn(|| {
        if let Ok(mut c) = pinyin_winpipe::connect(300) {
            let pid = std::process::id();
            if let Ok(Reply::Hello { proto }) = pinyin_ipc::call(&mut c, &Request::Hello { proto: PROTO, pid }) {
                if proto == PROTO {
                    let _ = pinyin_ipc::call(&mut c, &Request::ReloadSettings);
                }
            }
        }
    });
}

struct Fonts {
    title: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    label: IDWriteTextFormat,
    desc: IDWriteTextFormat,
    seg: IDWriteTextFormat,
    foot: IDWriteTextFormat,
}

struct App {
    hwnd: HWND,
    d2d: ID2D1Factory,
    fonts: Fonts,
    rt: Option<ID2D1HwndRenderTarget>,
    icon: Option<ID2D1Bitmap>,
    settings: Settings,
    dark: bool,
    /// 键盘焦点：0 文风，1 中文标点；用鼠标时不显示焦点框
    focus: Option<usize>,
    error: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn fonts(dw: &IDWriteFactory) -> Result<Fonts> {
    let mk = |size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT| -> Result<IDWriteTextFormat> {
        unsafe {
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
            f.SetTextAlignment(align)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            Ok(f)
        }
    };
    Ok(Fonts {
        title: mk(20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING)?,
        sub: mk(12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING)?,
        label: mk(15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING)?,
        desc: mk(12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING)?,
        seg: mk(14.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?,
        foot: mk(12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER)?,
    })
}

/// 从安装目录读图标图片（pinyin.png）。
fn load_icon(rt: &ID2D1RenderTarget) -> Option<ID2D1Bitmap> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let path = HSTRING::from(exe_dir().join("pinyin.png").as_os_str());
        let dec = wic.CreateDecoderFromFilename(&path, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )
        .ok()?;
        rt.CreateBitmapFromWicBitmap(&conv, None).ok()
    }
}

fn round(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}

fn inset(r: D2D_RECT_F, d: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left + d, top: r.top + d, right: r.right - d, bottom: r.bottom - d }
}

impl App {
    fn dpi(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd).max(96) as f32 }
    }

    fn apply_title_bar(&self) {
        unsafe {
            let dark: i32 = self.dark as i32;
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            // 标题栏和窗口背景同色（Windows 11）
            let c = theme(self.dark).bg;
            let cr = COLORREF(((c.b * 255.0) as u32) << 16 | ((c.g * 255.0) as u32) << 8 | (c.r * 255.0) as u32);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_CAPTION_COLOR, &cr as *const _ as *const _, 4);
        }
    }

    fn text(&self, rt: &ID2D1RenderTarget, s: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush) {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe { rt.DrawText(&w, f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL) };
    }

    fn paint(&mut self) -> Result<()> {
        let dpi = self.dpi();
        unsafe {
            let px = D2D_SIZE_U { width: (W * dpi / 96.0).ceil() as u32, height: (H * dpi / 96.0).ceil() as u32 };
            if self.rt.is_none() {
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    // 软件渲染：不加载显卡驱动，内存占用小
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
                let rt = self.d2d.CreateHwndRenderTarget(&props, &hp)?;
                self.icon = load_icon(&rt.clone().into());
                self.rt = Some(rt);
            }
            let hrt = self.rt.clone().unwrap();
            let _ = hrt.Resize(&px);
            hrt.SetDpi(dpi, dpi);
            let rt: ID2D1RenderTarget = hrt.clone().into();
            let th = theme(self.dark);
            let br = |c: D2D1_COLOR_F| rt.CreateSolidColorBrush(&c, None);
            let (text, dim, accent, track, knob, card) =
                (br(th.text)?, br(th.dim)?, br(th.accent)?, br(th.track)?, br(th.knob)?, br(th.card)?);

            rt.BeginDraw();
            rt.Clear(Some(&th.bg));

            // 标题
            let icon_r = D2D_RECT_F { left: 24.0, top: 24.0, right: 72.0, bottom: 72.0 };
            match &self.icon {
                Some(bmp) => rt.DrawBitmap(bmp, Some(&icon_r), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                None => rt.FillRoundedRectangle(&round(icon_r, 11.0), &text),
            }
            self.text(
                &rt,
                "拼音输入法",
                &self.fonts.title,
                D2D_RECT_F { left: 86.0, top: 24.0, right: W, bottom: 52.0 },
                &text,
            );
            self.text(&rt, "设置", &self.fonts.sub, D2D_RECT_F { left: 87.0, top: 52.0, right: W, bottom: 72.0 }, &dim);

            // 第一行：文风
            rt.FillRoundedRectangle(&round(CARD1, 10.0), &card);
            self.text(
                &rt,
                "文风",
                &self.fonts.label,
                D2D_RECT_F { left: 34.0, top: CARD1.top + 14.0, right: W, bottom: CARD1.top + 40.0 },
                &text,
            );
            self.text(
                &rt,
                "日常适合聊天，书面适合写文章",
                &self.fonts.desc,
                D2D_RECT_F { left: 34.0, top: CARD1.top + 42.0, right: W, bottom: CARD1.top + 64.0 },
                &dim,
            );
            let s = seg_rect();
            rt.FillRoundedRectangle(&round(s, SEG_H / 2.0), &track);
            let mid = (s.left + s.right) / 2.0;
            let (l, r) = (D2D_RECT_F { right: mid, ..s }, D2D_RECT_F { left: mid, ..s });
            let sel = if self.settings.formal { r } else { l };
            rt.FillRoundedRectangle(&round(inset(sel, 3.0), (SEG_H - 6.0) / 2.0), &accent);
            let (dc, fc) = if self.settings.formal { (&text, &knob) } else { (&knob, &text) };
            self.text(&rt, "日常", &self.fonts.seg, l, dc);
            self.text(&rt, "书面", &self.fonts.seg, r, fc);
            if self.focus == Some(0) {
                rt.DrawRoundedRectangle(&round(inset(s, -3.0), SEG_H / 2.0 + 3.0), &accent, 1.5, None);
            }

            // 第二行：中文标点
            rt.FillRoundedRectangle(&round(CARD2, 10.0), &card);
            self.text(
                &rt,
                "中文标点",
                &self.fonts.label,
                D2D_RECT_F { left: 34.0, top: CARD2.top + 14.0, right: W, bottom: CARD2.top + 40.0 },
                &text,
            );
            let desc = if self.settings.chinese_punct {
                "开：，。？！：；全部用中文标点"
            } else {
                "关：, . ? ! : ; 全部用英文标点"
            };
            self.text(
                &rt,
                desc,
                &self.fonts.desc,
                D2D_RECT_F { left: 34.0, top: CARD2.top + 42.0, right: W, bottom: CARD2.top + 64.0 },
                &dim,
            );
            let sw = switch_rect();
            let on = self.settings.chinese_punct;
            rt.FillRoundedRectangle(&round(sw, SW_H / 2.0), if on { &accent } else { &track });
            let kr = SW_H / 2.0 - 4.0;
            let kx = if on { sw.right - 4.0 - kr } else { sw.left + 4.0 + kr };
            let ell = D2D1_ELLIPSE {
                point: windows_numerics::Vector2 { X: kx, Y: (sw.top + sw.bottom) / 2.0 },
                radiusX: kr,
                radiusY: kr,
            };
            rt.FillEllipse(&ell, &knob);
            if self.focus == Some(1) {
                rt.DrawRoundedRectangle(&round(inset(sw, -3.0), SW_H / 2.0 + 3.0), &accent, 1.5, None);
            }

            // 底部
            let foot = if self.error {
                "保存失败：%APPDATA%\\PinyinIME 写不进去"
            } else {
                "改动立即生效 · 单按 Shift 切换中英"
            };
            self.text(
                &rt,
                foot,
                &self.fonts.foot,
                D2D_RECT_F { left: 0.0, top: CARD2.bottom + 14.0, right: W, bottom: H - 10.0 },
                if self.error { &accent } else { &dim },
            );

            if rt.EndDraw(None, None).is_err() {
                self.rt = None;
                self.icon = None;
            }
        }
        Ok(())
    }

    fn set(&mut self, s: Settings) {
        if s == self.settings {
            return;
        }
        self.settings = s;
        self.error = s.save(&settings_path()).is_err();
        if !self.error {
            ping_engine();
        }
        self.invalidate();
    }

    fn act(&mut self, h: Hit) {
        let mut s = self.settings;
        match h {
            Hit::Daily => s.formal = false,
            Hit::Formal => s.formal = true,
            Hit::Punct => s.chinese_punct = !s.chinese_punct,
        }
        self.set(s);
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn key(&mut self, vk: u16) {
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        match vk {
            v if v == VK_TAB.0 => {
                // 只有两项：Tab / Shift+Tab 都是换到另一项
                self.focus = Some(match self.focus {
                    Some(i) => 1 - i,
                    None => shift as usize,
                });
                self.invalidate();
            }
            v if v == VK_LEFT.0 || v == VK_RIGHT.0 => {
                match self.focus.unwrap_or(0) {
                    0 => self.act(if v == VK_LEFT.0 { Hit::Daily } else { Hit::Formal }),
                    _ => {
                        let mut s = self.settings;
                        s.chinese_punct = v == VK_RIGHT.0;
                        self.set(s);
                    }
                }
                if self.focus.is_none() {
                    self.focus = Some(0);
                    self.invalidate();
                }
            }
            v if v == VK_SPACE.0 || v == VK_RETURN.0 => match self.focus {
                Some(0) => self.act(if self.settings.formal { Hit::Daily } else { Hit::Formal }),
                Some(_) => self.act(Hit::Punct),
                None => {
                    self.focus = Some(0);
                    self.invalidate();
                }
            },
            _ => {}
        }
    }
}

fn with_app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn dip(lp: LPARAM, dpi: f32) -> (f32, f32) {
    let x = (lp.0 & 0xFFFF) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
    (x * 96.0 / dpi, y * 96.0 / dpi)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                with_app(|a| a.paint());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                with_app(|a| a.invalidate());
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                with_app(|a| {
                    let (x, y) = dip(lp, a.dpi());
                    a.focus = None;
                    match hit(x, y) {
                        Some(h) => a.act(h),
                        None => a.invalidate(),
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEMOVE => LRESULT(0),
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                let over = with_app(|a| {
                    let mut p = Default::default();
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
                    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(a.hwnd, &mut p);
                    let d = a.dpi();
                    hit(p.x as f32 * 96.0 / d, p.y as f32 * 96.0 / d).is_some()
                })
                .unwrap_or(false);
                let c: HCURSOR = LoadCursorW(None, if over { IDC_HAND } else { IDC_ARROW }).unwrap_or_default();
                SetCursor(Some(c));
                LRESULT(1)
            }
            WM_KEYDOWN => {
                if wp.0 as u16 == VK_ESCAPE.0 {
                    PostQuitMessage(0);
                } else {
                    with_app(|a| a.key(wp.0 as u16));
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                // 拖到缩放比例不同的显示器上：按系统建议的大小重新摆
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                with_app(|a| a.invalidate());
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                // 深色 / 浅色切换了
                with_app(|a| {
                    let d = system_dark();
                    if d != a.dark {
                        a.dark = d;
                        a.apply_title_bar();
                        a.invalidate();
                    }
                });
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// 把输入法加进（或移出）当前用户的输入法列表（安装、卸载程序以你的身份调用）。
/// 用系统的 input.dll 里的 InstallLayoutOrTip，和在"设置 → 语言"里手动添加效果一样。
fn install_tip(add: bool) -> bool {
    type InstallLayoutOrTip = unsafe extern "system" fn(PCWSTR, u32) -> BOOL;
    const ILOT_UNINSTALL: u32 = 1;
    let spec = format!(
        "0804:{}{}",
        pinyin_types::guid_string(pinyin_types::TIP_CLSID),
        pinyin_types::guid_string(pinyin_types::TIP_PROFILE)
    );
    unsafe {
        let Ok(lib) = LoadLibraryExW(w!("input.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) else { return false };
        let Some(f) = GetProcAddress(lib, s!("InstallLayoutOrTip")) else { return false };
        let f: InstallLayoutOrTip = std::mem::transmute(f);
        let spec = HSTRING::from(spec);
        f(PCWSTR(spec.as_ptr()), if add { 0 } else { ILOT_UNINSTALL }).as_bool()
    }
}

pub fn run() {
    match std::env::args().nth(1).as_deref() {
        Some("--enable") => std::process::exit(if install_tip(true) { 0 } else { 1 }),
        Some("--disable") => std::process::exit(if install_tip(false) { 0 } else { 1 }),
        _ => {}
    }
    unsafe {
        // 已经开着一个：把它拿到前面来
        if let Ok(h) = FindWindowW(CLASS, None) {
            if !h.is_invalid() {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
                return;
            }
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(inst) = GetModuleHandleW(None) else { return };
        let ico = exe_dir().join("pinyin.ico");
        let icon =
            LoadImageW(None, &HSTRING::from(ico.as_os_str()), IMAGE_ICON, 0, 0, LR_LOADFROMFILE | LR_DEFAULTSIZE)
                .map(|h| HICON(h.0))
                .unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let Ok(hwnd) = CreateWindowExW(
            Default::default(),
            CLASS,
            w!("拼音输入法设置"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            480,
            360,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return;
        };
        // 按这块屏幕的缩放比例定大小，放在屏幕正中
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut r =
            RECT { left: 0, top: 0, right: (W * dpi as f32 / 96.0) as i32, bottom: (H * dpi as f32 / 96.0) as i32 };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, Default::default(), dpi);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
        let wa = mi.rcWork;
        let x = wa.left + ((wa.right - wa.left) - w) / 2;
        let y = wa.top + ((wa.bottom - wa.top) - h) / 2;
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);

        let Ok(d2d) = D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) else { return };
        let Ok(dw) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else { return };
        let Ok(fonts) = fonts(&dw) else { return };
        let app = App {
            hwnd,
            d2d,
            fonts,
            rt: None,
            icon: None,
            settings: Settings::load(&settings_path()),
            dark: system_dark(),
            focus: None,
            error: false,
        };
        app.apply_title_bar();
        APP.with(|a| *a.borrow_mut() = Some(app));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetFocus(Some(hwnd));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
