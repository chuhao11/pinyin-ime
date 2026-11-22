//! 按程序声明的"输入框类型"分类（和平台无关，在 Mac 上也能测）：
//! - 密码框、PIN、纯数字框：输入法完全不碰，按键一个都不发给引擎（密码绝不离开这个程序）
//! - 隐私输入框（浏览器的无痕/InPrivate 窗口、要求"别学习"的输入框）：照常打中文，但什么都不记
//! - 其余：正常

/// Windows 的输入框类型编号（InputScope）。
pub mod scope {
    pub const URL: i32 = 1;
    pub const EMAIL: i32 = 5;
    pub const DIGITS: i32 = 28;
    pub const NUMBER: i32 = 29;
    pub const PASSWORD: i32 = 31;
    pub const PRIVATE: i32 = 61;
    pub const NUMERIC_PASSWORD: i32 = 63;
    pub const NUMERIC_PIN: i32 = 64;
    pub const ALPHANUMERIC_PIN: i32 = 65;
    pub const ALPHANUMERIC_PIN_SET: i32 = 66;
}

/// 输入框是哪一种。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Normal,
    /// 照常打字，但不学习、不往硬盘写任何东西
    Private,
    /// 密码框、PIN：输入法完全不碰，按键不发给引擎，诊断日志也不记这里按了什么键
    Secret,
    /// 输入法不工作（原因给诊断日志）
    Off(&'static str),
}

impl Kind {
    /// 输入法在这里不工作
    pub fn is_off(self) -> bool {
        matches!(self, Kind::Secret | Kind::Off(_))
    }
}

/// 按输入框声明的类型分类。几种类型同时出现时，"不工作"优先于"隐私"。
pub fn classify(scopes: &[i32]) -> Kind {
    use scope::*;
    let has = |s: i32| scopes.contains(&s);
    if [PASSWORD, NUMERIC_PASSWORD, NUMERIC_PIN, ALPHANUMERIC_PIN, ALPHANUMERIC_PIN_SET].into_iter().any(has) {
        return Kind::Secret;
    }
    if [DIGITS, NUMBER].into_iter().any(has) {
        return Kind::Off("数字框");
    }
    if has(PRIVATE) {
        return Kind::Private;
    }
    Kind::Normal
}

#[cfg(test)]
mod tests {
    use super::scope::*;
    use super::*;

    #[test]
    fn password_and_pin_boxes_are_off_limits() {
        for s in [PASSWORD, NUMERIC_PASSWORD, NUMERIC_PIN, ALPHANUMERIC_PIN, ALPHANUMERIC_PIN_SET] {
            assert_eq!(classify(&[s]), Kind::Secret, "{s}");
        }
        // 无痕窗口里的密码框：仍然完全不碰
        assert_eq!(classify(&[PRIVATE, PASSWORD]), Kind::Secret);
        assert!(matches!(classify(&[DIGITS]), Kind::Off(_)));
        assert!(matches!(classify(&[NUMBER]), Kind::Off(_)));
        for k in [Kind::Secret, Kind::Off("x")] {
            assert!(k.is_off());
        }
        assert!(!Kind::Private.is_off() && !Kind::Normal.is_off());
    }

    #[test]
    fn private_boxes_type_but_do_not_learn() {
        assert_eq!(classify(&[PRIVATE]), Kind::Private);
        // Chrome 无痕窗口里的网址栏、搜索框：类型后面跟着"隐私"
        assert_eq!(classify(&[URL, PRIVATE]), Kind::Private);
    }

    #[test]
    fn ordinary_boxes_are_normal() {
        assert_eq!(classify(&[]), Kind::Normal);
        assert_eq!(classify(&[0]), Kind::Normal);
        assert_eq!(classify(&[URL]), Kind::Normal);
        assert_eq!(classify(&[EMAIL]), Kind::Normal);
    }
}
