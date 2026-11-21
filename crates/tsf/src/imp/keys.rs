//! 把 Windows 的虚拟键码翻译成引擎的按键；识别"单独按一下 Shift"。

use std::time::{Duration, Instant};

use pinyin_types::{Key, Mods};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, GetKeyboardState, MapVirtualKeyW, ToUnicode, MAPVK_VK_TO_VSC, VIRTUAL_KEY, VK_BACK,
    VK_CAPITAL, VK_CONTROL, VK_DIVIDE, VK_DOWN, VK_ESCAPE, VK_LBUTTON, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT,
    VK_LWIN, VK_MBUTTON, VK_MENU, VK_NUMLOCK, VK_NUMPAD0, VK_RBUTTON, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SCROLL, VK_SHIFT, VK_SPACE, VK_UP, VK_XBUTTON1, VK_XBUTTON2,
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

/// 虚拟键码 → 按键。修饰键返回 None（永远不拦）。
pub fn translate(vk: u32, lparam: isize) -> Option<Key> {
    if is_modifier(vk) {
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

/// 单独按一下 Shift：按下到松开不超过 0.5 秒，中间没有按别的键，也没有点鼠标。
/// Shift+点鼠标选文字、按住 Shift 很久、Shift+别的键（包括 Ctrl+Shift、Alt+Shift 切换输入法）都不算。
#[derive(Default)]
pub struct ShiftTracker {
    down_at: Option<Instant>,
    spoiled: bool,
}

const TAP: Duration = Duration::from_millis(500);

fn mouse_clicked_since_last_check() -> bool {
    unsafe {
        [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2]
            .iter()
            .any(|b| GetAsyncKeyState(b.0 as i32) as u16 & 0x8001 != 0)
    }
}

impl ShiftTracker {
    pub fn shift_down(&mut self) {
        if self.down_at.is_none() {
            self.down_at = Some(Instant::now());
            // 清掉"按下过"的记录，之后再查就是 Shift 按着期间有没有点鼠标；鼠标正按着（拖选）也不算
            let _ = mouse_clicked_since_last_check();
            self.spoiled =
                unsafe { [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].iter().any(|b| GetAsyncKeyState(b.0 as i32) < 0) };
        }
    }

    pub fn other_key(&mut self) {
        if self.down_at.is_some() {
            self.spoiled = true;
        }
    }

    /// Shift 松开：是不是一次"单独按一下"。
    pub fn shift_up(&mut self) -> bool {
        let Some(t) = self.down_at.take() else { return false };
        let clicked = mouse_clicked_since_last_check();
        let tap = !self.spoiled && !clicked && t.elapsed() <= TAP;
        self.spoiled = false;
        tap
    }
}
