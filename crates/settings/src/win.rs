//! 设置页窗口：Direct2D 软件渲染 + DirectWrite；跟随 Windows 的深色 / 浅色；高分屏清晰。
//!
//! ┌───────────────────────────────────────────┐
//! │ [图标]  拼音输入法                          │
//! │         设置                                │
//! │ ┌───────────────────────────────────────┐ │
//! │ │ 中文标点                       (  ●)   │ │
//! │ │ 关：, . ? ! 全部用英文标点              │ │
//! │ └───────────────────────────────────────┘ │
//! │ ┌───────────────────────────────────────┐ │
//! │ │ 添加词语                                │ │
//! │ │ 用逗号、空格或换行隔开；多音字写成…     │ │
//! │ │ ┌───────────────────────────────────┐ │ │
//! │ │ │ 输入框                            │ │ │
//! │ │ └───────────────────────────────────┘ │ │
//! │ │ 已加入 3 个                    [加入]  │ │
//! │ └───────────────────────────────────────┘ │
//! │       改动立即生效 · 单按 Shift 切换中英      │
//! └───────────────────────────────────────────┘

use std::cell::RefCell;
use std::sync::Mutex;

use pinyin_ipc::{settings_path, user_dir, Reply, Request, Settings, PENDING_WORDS, PROTO};
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
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, GetMonitorInfoW, InvalidateRect,
    MonitorFromWindow, SetBkColor, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, FW_NORMAL, HBRUSH, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS,
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
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetMessageW, GetWindowTextLengthW, GetWindowTextW,
    LoadCursorW, LoadImageW, PostMessageW, PostQuitMessage, RegisterClassExW, SendMessageW, SetCursor,
    SetForegroundWindow, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, ES_AUTOVSCROLL, ES_MULTILINE, ES_WANTRETURN, HCURSOR, HICON, HTCLIENT, IDC_ARROW, IDC_HAND,
    IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, MSG, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE, SW_SHOW, WINDOW_STYLE,
    WM_APP, WM_CTLCOLOREDIT, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_PAINT,
    WM_SETCURSOR, WM_SETFONT, WM_SETTINGCHANGE, WM_SIZE, WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

const CLASS: PCWSTR = w!("PinyinIMESettings");
const FONT: &str = "Microsoft YaHei UI";
/// 后台加词完成
const WM_WORDS_DONE: u32 = WM_APP + 1;
/// 输入框最多多少字
const MAX_INPUT: usize = 8000;

/// 窗口内容大小（DIP）
const W: f32 = 460.0;
const H: f32 = 508.0;

/// 第一张卡片：中文标点
const CARD_P: D2D_RECT_F = D2D_RECT_F { left: 16.0, top: 96.0, right: W - 16.0, bottom: 176.0 };
/// 第二张卡片：添加词语
const CARD_A: D2D_RECT_F = D2D_RECT_F { left: 16.0, top: 188.0, right: W - 16.0, bottom: 462.0 };
/// 输入框的底（圆角框），真正的输入框在里面留 8 的边
const FIELD: D2D_RECT_F = D2D_RECT_F { left: 32.0, top: 256.0, right: W - 32.0, bottom: 396.0 };
const BUTTON: D2D_RECT_F = D2D_RECT_F { left: W - 32.0 - 88.0, top: 410.0, right: W - 32.0, bottom: 444.0 };
const STATUS: D2D_RECT_F = D2D_RECT_F { left: 34.0, top: 404.0, right: W - 32.0 - 88.0 - 12.0, bottom: 450.0 };
const SW_W: f32 = 50.0;
const SW_H: f32 = 28.0;

fn switch_rect() -> D2D_RECT_F {
    let cy = (CARD_P.top + CARD_P.bottom) / 2.0;
    D2D_RECT_F {
        left: CARD_P.right - 18.0 - SW_W,
        top: cy - SW_H / 2.0,
        right: CARD_P.right - 18.0,
        bottom: cy + SW_H / 2.0,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Punct,
    Add,
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

fn hit(x: f32, y: f32) -> Option<Hit> {
    // 整张"中文标点"卡片都能点
    if inside(&CARD_P, x, y) {
        return Some(Hit::Punct);
    }
    if inside(&BUTTON, x, y) {
        return Some(Hit::Add);
    }
    None
}

struct Theme {
    bg: D2D1_COLOR_F,
    card: D2D1_COLOR_F,
    field: D2D1_COLOR_F,
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

fn colorref(c: D2D1_COLOR_F) -> COLORREF {
    COLORREF(((c.b * 255.0) as u32) << 16 | ((c.g * 255.0) as u32) << 8 | (c.r * 255.0) as u32)
}

fn theme(dark: bool) -> Theme {
    if dark {
        Theme {
            bg: rgb(0x121212),
            card: rgb(0x1D1D1D),
            field: rgb(0x141414),
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
            field: rgb(0xF6F5F3),
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

/// 连上引擎（等 `timeout_ms`）。
fn engine(timeout_ms: u32) -> Option<pinyin_winpipe::PipeConn> {
    let mut c = pinyin_winpipe::connect(timeout_ms).ok()?;
    let pid = std::process::id();
    match pinyin_ipc::call(&mut c, &Request::Hello { proto: PROTO, pid }) {
        Ok(Reply::Hello { proto }) if proto == PROTO => Some(c),
        _ => None,
    }
}

/// 告诉引擎马上重新读设置（引擎没在运行也没关系：它启动时会读，平时两秒内也会自己发现）。
fn ping_engine() {
    std::thread::spawn(|| {
        if let Some(mut c) = engine(300) {
            let _ = pinyin_ipc::call(&mut c, &Request::ReloadSettings);
        }
    });
}

/// 后台加词的结果
enum Outcome {
    Added {
        added: u32,
        skipped: Vec<String>,
    },
    /// 引擎没在运行：先存起来，引擎启动时读
    Saved,
    Failed,
}

static OUTCOME: Mutex<Option<Outcome>> = Mutex::new(None);

fn send_words(text: &str) -> Outcome {
    if let Some(mut c) = engine(5000) {
        if let Ok(Reply::WordsAdded { added, skipped }) =
            pinyin_ipc::call(&mut c, &Request::AddWords { text: text.to_string() })
        {
            return Outcome::Added { added, skipped };
        }
    }
    // 引擎没在运行（或者是旧版本）：写进等待文件，引擎下次启动时加入
    let path = user_dir().join(PENDING_WORDS);
    let ok = (|| -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(user_dir())?;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{text}")
    })();
    if ok.is_ok() {
        // 引擎正好在启动：让它读一下
        ping_engine();
        Outcome::Saved
    } else {
        Outcome::Failed
    }
}

struct Fonts {
    title: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    label: IDWriteTextFormat,
    desc: IDWriteTextFormat,
    status: IDWriteTextFormat,
    button: IDWriteTextFormat,
    foot: IDWriteTextFormat,
}

struct App {
    hwnd: HWND,
    edit: HWND,
    edit_font: HFONT,
    edit_brush: HBRUSH,
    d2d: ID2D1Factory,
    fonts: Fonts,
    rt: Option<ID2D1HwndRenderTarget>,
    icon: Option<ID2D1Bitmap>,
    settings: Settings,
    dark: bool,
    /// 键盘焦点在"中文标点"开关上（按 Tab 切到这里）
    focus: bool,
    error: bool,
    /// 加词结果（显示在"加入"按钮左边）
    status: String,
    status_bad: bool,
    busy: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn fonts(dw: &IDWriteFactory) -> Result<Fonts> {
    let mk = |size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT, wrap: DWRITE_WORD_WRAPPING| unsafe {
        let f = dw.CreateTextFormat(
            &HSTRING::from(FONT),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("zh-CN"),
        )?;
        f.SetWordWrapping(wrap)?;
        f.SetTextAlignment(align)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok::<IDWriteTextFormat, windows::core::Error>(f)
    };
    let (n, b) = (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD);
    let (lead, center) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER);
    let (nowrap, wrap) = (DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP);
    Ok(Fonts {
        title: mk(20.0, b, lead, nowrap)?,
        sub: mk(12.5, n, lead, nowrap)?,
        label: mk(15.0, b, lead, nowrap)?,
        desc: mk(12.5, n, lead, nowrap)?,
        status: mk(12.5, n, lead, wrap)?,
        button: mk(14.0, b, center, nowrap)?,
        foot: mk(12.0, n, center, nowrap)?,
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

/// 跳过的词里，值得放回输入框让你改的（"已经加过了"的不用）
fn retry_words(skipped: &[String]) -> String {
    skipped
        .iter()
        .filter(|x| !x.contains("已经加过了") && !x.starts_with("一次最多"))
        .filter_map(|x| x.split('（').next())
        .collect::<Vec<_>>()
        .join("，")
}

impl App {
    fn dpi(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd).max(96) as f32 }
    }

    fn px(&self, v: f32) -> i32 {
        (v * self.dpi() / 96.0).round() as i32
    }

    fn apply_title_bar(&self) {
        unsafe {
            let dark: i32 = self.dark as i32;
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            // 标题栏和窗口背景同色（Windows 11）
            let cr = colorref(theme(self.dark).bg);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_CAPTION_COLOR, &cr as *const _ as *const _, 4);
        }
    }

    /// 输入框的颜色、字体、位置（深浅色切换、换了缩放比例的显示器时重新设置）
    fn style_edit(&mut self) {
        unsafe {
            if !self.edit_brush.is_invalid() {
                let _ = DeleteObject(self.edit_brush.into());
            }
            self.edit_brush = CreateSolidBrush(colorref(theme(self.dark).field));
            let _ = SetWindowTheme(self.edit, if self.dark { w!("DarkMode_Explorer") } else { w!("Explorer") }, None);
            if !self.edit_font.is_invalid() {
                let _ = DeleteObject(self.edit_font.into());
            }
            self.edit_font = CreateFontW(
                -self.px(14.0),
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                DEFAULT_PITCH.0 as u32,
                &HSTRING::from(FONT),
            );
            SendMessageW(self.edit, WM_SETFONT, Some(WPARAM(self.edit_font.0 as usize)), Some(LPARAM(1)));
            let r = inset(FIELD, 8.0);
            let _ = SetWindowPos(
                self.edit,
                None,
                self.px(r.left),
                self.px(r.top),
                self.px(r.right - r.left),
                self.px(r.bottom - r.top),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = InvalidateRect(Some(self.edit), None, true);
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
            let (text, dim, accent, track, knob, card, field) =
                (br(th.text)?, br(th.dim)?, br(th.accent)?, br(th.track)?, br(th.knob)?, br(th.card)?, br(th.field)?);
            let rect = |l: f32, t: f32, r: f32, b: f32| D2D_RECT_F { left: l, top: t, right: r, bottom: b };

            rt.BeginDraw();
            rt.Clear(Some(&th.bg));

            // 标题
            let icon_r = rect(24.0, 24.0, 72.0, 72.0);
            match &self.icon {
                Some(bmp) => rt.DrawBitmap(bmp, Some(&icon_r), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                None => rt.FillRoundedRectangle(&round(icon_r, 11.0), &text),
            }
            self.text(&rt, "拼音输入法", &self.fonts.title, rect(86.0, 24.0, W, 52.0), &text);
            self.text(&rt, "设置", &self.fonts.sub, rect(87.0, 52.0, W, 72.0), &dim);

            // 中文标点
            rt.FillRoundedRectangle(&round(CARD_P, 10.0), &card);
            self.text(&rt, "中文标点", &self.fonts.label, rect(34.0, CARD_P.top + 14.0, W, CARD_P.top + 40.0), &text);
            let desc = if self.settings.chinese_punct {
                "开：，。？！：；全部用中文标点"
            } else {
                "关：, . ? ! : ; 全部用英文标点"
            };
            self.text(&rt, desc, &self.fonts.desc, rect(34.0, CARD_P.top + 42.0, W, CARD_P.top + 64.0), &dim);
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
            if self.focus {
                rt.DrawRoundedRectangle(&round(inset(sw, -3.0), SW_H / 2.0 + 3.0), &accent, 1.5, None);
            }

            // 添加词语
            rt.FillRoundedRectangle(&round(CARD_A, 10.0), &card);
            self.text(&rt, "添加词语", &self.fonts.label, rect(34.0, CARD_A.top + 14.0, W, CARD_A.top + 40.0), &text);
            self.text(
                &rt,
                "用逗号、空格或换行隔开；多音字可以写成 银行(yin hang)",
                &self.fonts.desc,
                rect(34.0, CARD_A.top + 42.0, W - 20.0, CARD_A.top + 62.0),
                &dim,
            );
            rt.FillRoundedRectangle(&round(FIELD, 8.0), &field);
            rt.DrawRoundedRectangle(&round(FIELD, 8.0), &track, 1.0, None);
            let status = if self.status.is_empty() {
                "加入的词用全拼打排第二，之后选得多就排第一"
            } else {
                &self.status
            };
            self.text(&rt, status, &self.fonts.status, STATUS, if self.status_bad { &accent } else { &dim });
            rt.FillRoundedRectangle(
                &round(BUTTON, (BUTTON.bottom - BUTTON.top) / 2.0),
                if self.busy { &track } else { &accent },
            );
            self.text(&rt, if self.busy { "加入中…" } else { "加入" }, &self.fonts.button, BUTTON, &knob);

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
                rect(0.0, CARD_A.bottom + 10.0, W, H - 8.0),
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

    fn toggle_punct(&mut self) {
        let mut s = self.settings;
        s.chinese_punct = !s.chinese_punct;
        self.set(s);
    }

    fn edit_text(&self) -> String {
        unsafe {
            let n = GetWindowTextLengthW(self.edit).max(0) as usize;
            let mut buf = vec![0u16; n + 1];
            let got = GetWindowTextW(self.edit, &mut buf).max(0) as usize;
            String::from_utf16_lossy(&buf[..got.min(n)])
        }
    }

    /// 点"加入"：在后台把词交给引擎（不卡界面），完成后发 WM_WORDS_DONE 回来
    fn submit(&mut self) {
        if self.busy {
            return;
        }
        let text: String = self.edit_text().chars().take(MAX_INPUT).collect();
        if text.trim().is_empty() {
            self.status = "先在上面输入要加的词".into();
            self.status_bad = true;
            self.invalidate();
            return;
        }
        self.busy = true;
        self.status = "正在加入…".into();
        self.status_bad = false;
        self.invalidate();
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let r = send_words(&text);
            *OUTCOME.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_WORDS_DONE, WPARAM(0), LPARAM(0));
            }
        });
    }

    fn words_done(&mut self) {
        self.busy = false;
        let Some(r) = OUTCOME.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        match r {
            Outcome::Added { added, skipped } => {
                let mut s = format!("已加入 {added} 个");
                if !skipped.is_empty() {
                    let shown: Vec<&str> = skipped.iter().take(3).map(|x| x.as_str()).collect();
                    let more = if skipped.len() > 3 { format!(" 等 {} 个", skipped.len()) } else { String::new() };
                    s.push_str(&format!("；跳过：{}{more}", shown.join("、")));
                }
                self.status_bad = added == 0 && !skipped.is_empty();
                self.status = s;
                // 没加进去的放回输入框，改了可以再加
                let retry = retry_words(&skipped);
                unsafe {
                    let _ = SetWindowTextW(self.edit, &HSTRING::from(retry));
                }
            }
            Outcome::Saved => {
                self.status = "引擎没在运行：已经记下，下次启动时自动加入".into();
                self.status_bad = false;
                unsafe {
                    let _ = SetWindowTextW(self.edit, w!(""));
                }
            }
            Outcome::Failed => {
                self.status = "加入失败：%APPDATA%\\PinyinIME 写不进去".into();
                self.status_bad = true;
            }
        }
        self.invalidate();
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn key(&mut self, vk: u16) {
        match vk {
            // Tab：在"中文标点"开关和输入框之间切换
            v if v == VK_TAB.0 => {
                self.focus = !self.focus;
                if !self.focus {
                    unsafe {
                        let _ = SetFocus(Some(self.edit));
                    }
                }
                self.invalidate();
            }
            v if self.focus && (v == VK_LEFT.0 || v == VK_RIGHT.0) => {
                let mut s = self.settings;
                s.chinese_punct = v == VK_RIGHT.0;
                self.set(s);
            }
            v if self.focus && (v == VK_SPACE.0 || v == VK_RETURN.0) => self.toggle_punct(),
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
            // 输入框的颜色跟着深浅色走
            WM_CTLCOLOREDIT => {
                let hdc = HDC(wp.0 as *mut _);
                with_app(|a| {
                    let th = theme(a.dark);
                    SetTextColor(hdc, colorref(th.text));
                    SetBkColor(hdc, colorref(th.field));
                    LRESULT(a.edit_brush.0 as isize)
                })
                .unwrap_or(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                with_app(|a| {
                    let (x, y) = dip(lp, a.dpi());
                    a.focus = false;
                    let _ = SetFocus(Some(a.hwnd));
                    match hit(x, y) {
                        Some(Hit::Punct) => a.toggle_punct(),
                        Some(Hit::Add) => a.submit(),
                        None => {
                            // 点输入框的底（留边的地方）：把光标放进输入框
                            if inside(&FIELD, x, y) {
                                let _ = SetFocus(Some(a.edit));
                            }
                            a.invalidate();
                        }
                    }
                });
                LRESULT(0)
            }
            WM_WORDS_DONE => {
                with_app(|a| a.words_done());
                LRESULT(0)
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT && HWND(wp.0 as *mut _) == hwnd => {
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
                with_app(|a| {
                    a.style_edit();
                    a.invalidate();
                });
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                // 深色 / 浅色切换了
                with_app(|a| {
                    let d = system_dark();
                    if d != a.dark {
                        a.dark = d;
                        a.apply_title_bar();
                        a.style_edit();
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
        // WS_CLIPCHILDREN：画界面时不盖住输入框
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let Ok(hwnd) = CreateWindowExW(
            Default::default(),
            CLASS,
            w!("拼音输入法设置"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            480,
            540,
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

        // 输入框：系统自带的多行输入框（打字、复制粘贴、输入法都正常）
        let es = WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32);
        let Ok(edit) = CreateWindowExW(
            Default::default(),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_TABSTOP | es,
            0,
            0,
            10,
            10,
            Some(hwnd),
            None,
            Some(inst.into()),
            None,
        ) else {
            return;
        };

        let Ok(d2d) = D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) else { return };
        let Ok(dw) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else { return };
        let Ok(fonts) = fonts(&dw) else { return };
        let mut app = App {
            hwnd,
            edit,
            edit_font: HFONT::default(),
            edit_brush: HBRUSH::default(),
            d2d,
            fonts,
            rt: None,
            icon: None,
            settings: Settings::load(&settings_path()),
            dark: system_dark(),
            focus: false,
            error: false,
            status: String::new(),
            status_bad: false,
            busy: false,
        };
        app.apply_title_bar();
        app.style_edit();
        APP.with(|a| *a.borrow_mut() = Some(app));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetFocus(Some(edit));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
