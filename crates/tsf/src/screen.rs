//! "现在是不是在全屏游戏里"的判断规则（和平台无关，在 Mac 上也能测）。
//!
//! 只看正在打字的那个窗口自己，不问"系统里有没有全屏程序"：
//! 接了副屏以后，另一块屏幕上的全屏视频、没有任务栏的副屏上最大化的窗口，
//! 都曾经让这一块屏幕上的输入框以为自己在游戏里，只能打英文。

/// 屏幕坐标里的一个矩形（左、上、右、下）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Rect { left, top, right, bottom }
    }

    fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

/// 缩放换算可能差一两个像素
const SLACK: i32 = 2;

/// 打字的这个窗口是不是铺满了它所在的那块屏幕（全屏）。
///
/// - `window`：窗口（最外层的那个）的位置；`monitor`：它所在那块屏幕的整个范围（含任务栏）
/// - `maximized`：窗口是最大化的。最大化是普通窗口，永远不算全屏——
///   没有任务栏的副屏上（或者任务栏设成自动隐藏时），最大化的窗口也会盖满整块屏幕
pub fn fills_screen(window: Rect, monitor: Rect, maximized: bool) -> bool {
    if maximized || window.is_empty() || monitor.is_empty() {
        return false;
    }
    window.left <= monitor.left + SLACK
        && window.top <= monitor.top + SLACK
        && window.right >= monitor.right - SLACK
        && window.bottom >= monitor.bottom - SLACK
}

/// 判断"全屏游戏"用到的几样东西。
#[derive(Clone, Copy, Debug, Default)]
pub struct Signals {
    /// 打字的窗口铺满了它所在的屏幕（见 [`fills_screen`]）
    pub fills_screen: bool,
    /// 输入框是"临时"的：老式程序（游戏一般是这种）由系统代管的输入框；Chrome 的输入框也这样标
    pub transitory: bool,
    /// 程序自己实现了文字框，还告诉系统"屏幕键盘由我来弹"：Chrome、Edge 和用它们做的程序
    /// 只在网页上真有输入框（弹幕框、搜索框）时才给出这种输入框，不会是游戏画面
    pub app_text_box: bool,
    /// 这个线程有系统光标：普通文字框都有，游戏画面一般没有
    pub caret: bool,
}

/// 全屏游戏：铺满屏幕，又不是真正的文字输入框。游戏里默认按键全交给游戏（WASD 不会变成打字）。
pub fn looks_like_game(s: Signals) -> bool {
    s.fills_screen && s.transitory && !s.app_text_box && !s.caret
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 主屏 1920×1080（任务栏在底下），副屏 2560×1440 接在左边（坐标是负的）
    const MAIN: Rect = Rect::new(0, 0, 1920, 1080);
    const SIDE: Rect = Rect::new(-2560, 0, 0, 1440);

    #[test]
    fn maximized_window_is_never_fullscreen() {
        // 没有任务栏的副屏上最大化：窗口比屏幕还大一圈（边框露在外面），但它只是普通窗口
        let w = Rect::new(-2568, -8, 8, 1448);
        assert!(!fills_screen(w, SIDE, true));
        // 主屏上任务栏自动隐藏时最大化，也一样
        assert!(!fills_screen(Rect::new(-8, -8, 1928, 1088), MAIN, true));
    }

    #[test]
    fn real_fullscreen_on_either_screen() {
        assert!(fills_screen(MAIN, MAIN, false));
        assert!(fills_screen(SIDE, SIDE, false));
        // 缩放换算差一个像素也算
        assert!(fills_screen(Rect::new(-2559, 1, -1, 1439), SIDE, false));
    }

    #[test]
    fn only_the_typing_window_counts() {
        // 副屏全屏放视频，主屏上一个普通窗口在打字：看的是主屏这个窗口，它没铺满
        let typing = Rect::new(200, 100, 1400, 900);
        assert!(!fills_screen(typing, MAIN, false));
        // 铺满主屏那么大，却放在副屏上：副屏比它大，不算
        assert!(!fills_screen(Rect::new(-2560, 0, -640, 1080), SIDE, false));
    }

    #[test]
    fn broken_numbers_mean_not_fullscreen() {
        assert!(!fills_screen(Rect::new(0, 0, 0, 0), MAIN, false));
        assert!(!fills_screen(MAIN, Rect::new(0, 0, 0, 0), false));
    }

    #[test]
    fn game_needs_every_sign() {
        let game = Signals { fills_screen: true, transitory: true, app_text_box: false, caret: false };
        assert!(looks_like_game(game));
        // 少任何一样都不是游戏
        assert!(!looks_like_game(Signals { fills_screen: false, ..game }));
        assert!(!looks_like_game(Signals { transitory: false, ..game }));
        assert!(!looks_like_game(Signals { caret: true, ..game }));
    }

    #[test]
    fn fullscreen_web_page_text_box_is_not_a_game() {
        // B 站全屏看视频时点开弹幕框：Chrome 铺满屏幕、输入框是"临时"的、没有系统光标，
        // 但它是网页上真正的输入框
        let danmaku = Signals { fills_screen: true, transitory: true, app_text_box: true, caret: false };
        assert!(!looks_like_game(danmaku));
    }
}
