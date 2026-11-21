//! 输入法各部分共用的简单类型：按键、修饰键、候选窗要显示的内容。
//! 单独放一个没有任何依赖的小库，这样每个程序里加载的 DLL 只需要它和通信协议，不用带上词库、数据库。

/// 输入法在 Windows 里的编号（COM 类）和"中文(简体)"下的配置编号。永远不变（变了会被当成另一个输入法）。
pub const TIP_CLSID: u128 = 0x4d576843_6f05_43dc_b08f_fc06ae6d83f7;
pub const TIP_PROFILE: u128 = 0x2f1ff890_9808_4af9_8ca1_32893cb7bd5e;

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` 形式（注册表、系统设置里用）。
pub fn guid_string(g: u128) -> String {
    let h = format!("{g:032X}");
    format!("{{{}-{}-{}-{}-{}}}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// 按键。`Char` 是这个键实际打出的字符（已经考虑了 Shift），比如 Shift+1 是 `Char('!')`。
/// `ShiftTap` 是"单独按一下 Shift"（按下再松开，中间没按别的键），用来切换中文 / 纯英文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Space,
    Enter,
    Backspace,
    Escape,
    Left,
    Right,
    Up,
    Down,
    /// 其他键（Tab、Home、F1……）
    Other,
    /// 单独按一下 Shift
    ShiftTap,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub caps_lock: bool,
}

/// 处理一个按键的结果。`consumed` 为假表示这个键原样交给程序。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub consumed: bool,
    pub commit: Option<String>,
}

impl Response {
    pub fn eaten() -> Self {
        Response { consumed: true, commit: None }
    }
    pub fn pass() -> Self {
        Response { consumed: false, commit: None }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// 候选条
    #[default]
    Row,
    /// 5×5 矩阵
    Grid,
}

/// 候选窗要显示的内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub composing: bool,
    /// 输入区：已选的字 + 还没转换的字母
    pub preedit: String,
    pub mode: Mode,
    /// 候选条模式：这一排的候选；矩阵模式：这一页（最多 25 个，每行 5 个）
    pub items: Vec<String>,
    /// 高亮的是 `items` 里的第几个
    pub highlight: usize,
    /// 矩阵第几页、共几页
    pub page: usize,
    pub pages: usize,
    pub total: usize,
    /// 和 `items` 一一对应：这个候选是不是打错纠正出来的
    pub corrected: Vec<bool>,
    /// 现在是纯英文输入
    pub english_mode: bool,
    /// 高亮的那个词已经按了一下 `：再按一下就删掉它（候选窗里这个词变成彩虹色）
    pub armed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_format() {
        assert_eq!(guid_string(TIP_CLSID), "{4D576843-6F05-43DC-B08F-FC06AE6D83F7}");
    }
}
