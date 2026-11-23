//! 把 Windows 的虚拟键码翻译成引擎的按键；识别"单独按一下 Shift"。

use std::cell::Cell;
use std::time::Instant;

use pinyin_types::{Key, Mods};

use crate::shift::ShiftTap;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, GetKeyboardState, MapVirtualKeyW, ToUnicode, MAPVK_VK_TO_VSC, VIRTUAL_KEY, VK_BACK,
    VK_CAPITAL, VK_CONTROL, VK_DIVIDE, VK_DOWN, VK_ESCAPE, VK_LBUTTON, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT,
    VK_LWIN, VK_MBUTTON, VK_MENU, VK_NUMLOCK, VK_NUMPAD0, VK_RBUTTON, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SCROLL, VK_SHIFT, VK_SPACE, VK_UP, VK_XBUTTON1, VK_XBUTTON2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, WH_MOUSE, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_NCLBUTTONDOWN, WM_NCMBUTTONDOWN, WM_NCRBUTTONDOWN, WM_NCXBUTTONDOWN, WM_RBUTTONDOWN, WM_XBUTTONDOWN,
};

pub fn is_shift(vk: u32) -> bool {
    vk == VK_SHIFT.0 as u32 || vk == VK_LSHIFT.0 as u32 || vk == VK_RSHIFT.0 as u32
}

/// 这些键单独按下时永远不拦（修饰键、锁定键）。
fn is_modifier(vk: u32) -> bool {
    [
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
        VK_LWIN,
        VK_RWIN,
        VK_CAPITAL,
        VK_NUMLOCK,
        VK_SCROLL,
    ]
    .iter()
    .any(|k| k.0 as u32 == vk)
        || is_shift(vk)
}

/// 修饰键状态。Win 键按着也算作 Ctrl：Win+字母是系统快捷键，永远交给系统。
pub fn mods() -> Mods {
    unsafe {
        let down = |k: VIRTUAL_KEY| GetKeyState(k.0 as i32) < 0;
        Mods {
            ctrl: down(VK_CONTROL) || down(VK_LWIN) || down(VK_RWIN),
            alt: down(VK_MENU),
            caps_lock: GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0,
        }
    }
}

/// 小键盘上的数字和运算符（0–9 . + - * /）。没在打拼音时它们永远原样输入：
/// 用小键盘打 1.5 不会变成"1。5"，小键盘的减号也不会打开候选矩阵。
pub fn is_numpad(vk: u32) -> bool {
    (VK_NUMPAD0.0 as u32..=VK_DIVIDE.0 as u32).contains(&vk)
}

/// 虚拟键码 → 按键。修饰键、功能键、音量媒体键这些返回 None（永远不拦）。
pub fn translate(vk: u32, lparam: isize) -> Option<Key> {
    if is_modifier(vk) || crate::gate::never_intercept(vk) {
        return None;
    }
    let k = match vk {
        v if v == VK_SPACE.0 as u32 => Key::Space,
        v if v == VK_RETURN.0 as u32 => Key::Enter,
        v if v == VK_BACK.0 as u32 => Key::Backspace,
        v if v == VK_ESCAPE.0 as u32 => Key::Escape,
        v if v == VK_LEFT.0 as u32 => Key::Left,
        v if v == VK_RIGHT.0 as u32 => Key::Right,
        v if v == VK_UP.0 as u32 => Key::Up,
        v if v == VK_DOWN.0 as u32 => Key::Down,
        _ => {
            // 用当前键盘布局算出这个键实际打出的字符（考虑 Shift、小键盘）。
            // 标志 4：不改变键盘状态（不会弄乱死键，比如法语键盘的重音）
            let mut state = [0u8; 256];
            unsafe {
                if GetKeyboardState(&mut state).is_err() {
                    return Some(Key::Other);
                }
                // Ctrl/Alt 按着时字符没有意义，交给引擎按"修饰键"处理（引擎会放过）
                let scan = ((lparam >> 16) & 0xFF) as u32;
                let scan = if scan == 0 { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) } else { scan };
                let mut buf = [0u16; 8];
                let n = ToUnicode(vk, scan, Some(&state), &mut buf, 4);
                if n == 1 {
                    if let Some(c) = char::from_u32(buf[0] as u32) {
                        if !c.is_control() {
                            return Some(Key::Char(c));
                        }
                    }
                }
            }
            Key::Other
        }
    };
    Some(k)
}

/// 单独按一下 Shift（规则见 shift.rs）：每一次真实的按键都算，连按多快都行；
/// 同一次松开系统和按键接口各报一次时只切一次。
///
/// 点鼠标用一个只看本线程的鼠标钩子检测（Shift 按着时才装，松开就拆）。
#[derive(Default)]
pub struct ShiftTracker {
    tap: ShiftTap,
    hook: Option<HHOOK>,
}

thread_local! {
    static CLICKED: Cell<bool> = const { Cell::new(false) };
}

extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let m = wp.0 as u32;
        if [
            WM_LBUTTONDOWN,
            WM_RBUTTONDOWN,
            WM_MBUTTONDOWN,
            WM_XBUTTONDOWN,
            WM_NCLBUTTONDOWN,
            WM_NCRBUTTONDOWN,
            WM_NCMBUTTONDOWN,
            WM_NCXBUTTONDOWN,
        ]
        .contains(&m)
        {
            CLICKED.with(|c| c.set(true));
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

fn any_mouse_button_down() -> bool {
    unsafe {
        [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2].iter().any(|b| GetAsyncKeyState(b.0 as i32) < 0)
    }
}

impl ShiftTracker {
    /// `repeat`：按住不放时系统重复发的"按下"（lParam 第 30 位）。
    pub fn shift_down(&mut self, repeat: bool) {
        if repeat && self.tap.held() {
            return;
        }
        self.tap.down(repeat, any_mouse_button_down(), Instant::now());
        CLICKED.with(|c| c.set(false));
        if self.hook.is_none() {
            self.hook = unsafe { SetWindowsHookExW(WH_MOUSE, Some(mouse_hook), None, GetCurrentThreadId()) }.ok();
        }
    }

    pub fn other_key(&mut self) {
        self.tap.spoil();
    }

    fn unhook(&mut self) {
        if let Some(h) = self.hook.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(h);
            }
        }
    }

    /// 按着的时候点过鼠标：这次不算单按
    fn note_click(&mut self) {
        if CLICKED.with(|c| c.replace(false)) {
            self.tap.spoil();
        }
    }

    /// 按键接口报 Shift 松开：要不要切换中英。
    pub fn key_up(&mut self) -> bool {
        self.note_click();
        let r = self.tap.key_up(any_mouse_button_down(), Instant::now());
        self.unhook();
        r
    }

    /// 系统的保留键检测报"单独按了一下 Shift"：要不要切换中英。
    pub fn preserved(&mut self) -> bool {
        self.note_click();
        let r = self.tap.preserved(any_mouse_button_down(), Instant::now());
        self.unhook();
        r
    }

    /// 停用输入法时拆掉钩子。
    pub fn reset(&mut self) {
        self.tap.reset();
        CLICKED.with(|c| c.set(false));
        self.unhook();
    }

    /// 给诊断日志用
    pub fn describe(&self) -> String {
        format!("{} 点了鼠标={}", self.tap.describe(Instant::now()), CLICKED.with(|c| c.get()))
    }
}

impl Drop for ShiftTracker {
    fn drop(&mut self) {
        self.unhook();
    }
}
