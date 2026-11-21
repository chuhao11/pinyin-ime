//! 按键的"按下"和"松开"配对：程序收到了按下，就一定收到松开。
//!
//! Windows 对每次按键会问输入法两遍：先问"要不要"（OnTestKeyDown），说要再真正处理（OnKeyDown）；
//! 松开也一样。可是不同程序调用的方式不一样：有的只问不处理，有的不问直接处理，有的同一个键问好几遍。
//! 这里把每次按键的决定记下来，保证：
//!
//! - 同一次按键不管被问几遍，只判断一次（引擎不会收到两遍）
//! - 只有"按下"被输入法吃掉的键，"松开"才吃掉。程序看到了按下，就一定看到松开。
//!   否则程序以为这个键一直按着：B 站把它当成长按方向键开了倍速、空格不暂停，游戏里人物一直往前跑。
//! - 按住不放（系统重复发"按下"）时只记一次，松开一次就配平，不会留下没配对的"松开"去吃掉以后的按键
//!
//! 这个文件不依赖 Windows，可以在任何系统上测试。

use std::time::{Duration, Instant};

/// 同一次按键的几次询问之间不会隔这么久
const SAME_EVENT: Duration = Duration::from_millis(500);

/// lParam 里认这次按键的部分：扫描码和扩展键标志（第 16–24 位）。
/// 重复次数、"之前是否按着"这些位在几次询问之间可能不一样。
fn scan(lp: isize) -> isize {
    (lp >> 16) & 0x1FF
}

#[derive(Default)]
pub struct KeyGate {
    /// 最近一次问过"按下要不要"、还没真正处理的：键码、扫描码、要不要、时间
    down: Option<(u32, isize, bool, Instant)>,
    /// 最近一次问过"松开要不要"的：键码、要不要、时间
    up: Option<(u32, bool, Instant)>,
    /// 按下被输入法吃掉、还没松开的键（每个键最多一份）
    held: Vec<u32>,
}

impl KeyGate {
    /// OnTestKeyDown 开始：这次按键刚问过，返回上次的决定（不再判断第二遍）。
    pub fn test_down_cached(&mut self, vk: u32, lp: isize, now: Instant) -> Option<bool> {
        self.up = None;
        match self.down {
            Some((v, s, eat, t)) if v == vk && s == scan(lp) && now.duration_since(t) < SAME_EVENT => Some(eat),
            _ => None,
        }
    }

    /// OnTestKeyDown 做了决定。不要的键，程序会收到它的按下，所以它的松开也一定要交给程序。
    pub fn tested_down(&mut self, vk: u32, lp: isize, eat: bool, now: Instant) {
        self.down = Some((vk, scan(lp), eat, now));
        if !eat {
            self.release(vk);
        }
    }

    /// OnKeyDown 开始：拿走 OnTestKeyDown 对这次按键的决定。没问过返回 None（要自己判断）。
    pub fn key_down_cached(&mut self, vk: u32, lp: isize, now: Instant) -> Option<bool> {
        self.up = None;
        match self.down.take() {
            Some((v, s, eat, t)) if v == vk && s == scan(lp) && now.duration_since(t) < SAME_EVENT => Some(eat),
            _ => None,
        }
    }

    /// OnKeyDown 处理完：`eaten` 是这次按下最后有没有被输入法吃掉。
    pub fn finished_down(&mut self, vk: u32, eaten: bool) {
        if eaten {
            if !self.held.contains(&vk) {
                self.held.push(vk);
            }
        } else {
            self.release(vk);
        }
    }

    /// OnTestKeyUp / OnKeyUp 开始：这次松开刚问过，返回上次的决定。`take`：用完就忘（OnKeyUp）。
    pub fn up_cached(&mut self, vk: u32, now: Instant, take: bool) -> Option<bool> {
        self.down = None;
        let hit = match self.up {
            Some((v, eat, t)) if v == vk && now.duration_since(t) < SAME_EVENT => Some(eat),
            _ => None,
        };
        if take {
            self.up = None;
        }
        hit
    }

    /// 松开：按下被吃掉的才吃掉（同时忘掉这个键）。
    pub fn decide_up(&mut self, vk: u32) -> bool {
        self.release(vk)
    }

    /// 记下 OnTestKeyUp 的决定（之后的 OnKeyUp 直接用）。
    pub fn tested_up(&mut self, vk: u32, eat: bool, now: Instant) {
        self.up = Some((vk, eat, now));
    }

    /// 换了输入框、停用输入法：全部忘掉。
    pub fn clear(&mut self) {
        *self = KeyGate::default();
    }

    /// 现在有哪些键的松开会被吃掉（测试用）。
    pub fn held(&self) -> &[u32] {
        &self.held
    }

    fn release(&mut self, vk: u32) -> bool {
        match self.held.iter().position(|&k| k == vk) {
            Some(i) => {
                self.held.swap_remove(i);
                true
            }
            None => false,
        }
    }
}

/// 永远不拦的键：功能键、音量和媒体键、浏览器键、截屏、菜单键、睡眠、
/// 别的程序模拟输入的字符（VK_PACKET）、日韩输入法专用键。
/// 打字打到一半按它们也直接交给程序（调音量、F5 刷新、截屏都照常）。
pub fn never_intercept(vk: u32) -> bool {
    matches!(vk,
        0x03                // Ctrl+Break
        | 0x13              // Pause
        | 0x15..=0x1A       // 日韩输入法的键（假名、韩文、汉字……）
        | 0x1C..=0x1F       // 日文输入法的 变换 / 无变换 等
        | 0x2C              // PrintScreen
        | 0x2F              // Help
        | 0x5D              // 菜单键
        | 0x5F              // 睡眠
        | 0x70..=0x87       // F1–F24
        | 0xA6..=0xB7       // 浏览器后退前进、音量、静音、播放暂停、上一首下一首、打开邮件/计算器
        | 0xE5              // 输入法正在处理的键
        | 0xE7              // 别的程序模拟输入的字符
        | 0xF6..=0xFE)
}

/// 诊断日志里按键的名字。只给不会打出字的键起名（打出字的键一律不记，免得日志里留下打的内容）。
pub fn key_label(vk: u32) -> Option<&'static str> {
    Some(match vk {
        0x08 => "退格",
        0x09 => "Tab",
        0x0D => "回车",
        0x1B => "Esc",
        0x20 => "空格",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "←",
        0x26 => "↑",
        0x27 => "→",
        0x28 => "↓",
        0x2E => "Delete",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: u32 = 0x20;
    const RIGHT: u32 = 0x27;
    const LEFT: u32 = 0x25;
    const A: u32 = 0x41;
    /// 扫描码放在第 16 位起
    fn lp(scan: isize, repeat: bool) -> isize {
        (scan << 16) | 1 | if repeat { 1 << 30 } else { 0 }
    }

    /// 一个程序：记下自己收到的按下和松开，最后检查每个收到的按下都有松开。
    #[derive(Default)]
    struct App {
        downs: Vec<u32>,
        ups: Vec<u32>,
    }

    /// 几种调用方式：先问再处理（大多数程序）、不问直接处理、问两遍再处理。
    #[derive(Clone, Copy, Debug)]
    enum Host {
        TestThenAct,
        ActOnly,
        TestTwice,
    }

    /// 模拟一次按下：`eat` 是输入法这次想不想要。返回输入法有没有吃掉。
    fn press(g: &mut KeyGate, host: Host, app: &mut App, vk: u32, l: isize, eat: bool, now: Instant) -> bool {
        let mut decided = 0;
        let mut decide = || {
            decided += 1;
            eat
        };
        let eaten = match host {
            Host::TestThenAct | Host::TestTwice => {
                let mut said = None;
                for _ in 0..if matches!(host, Host::TestTwice) { 2 } else { 1 } {
                    let e = match g.test_down_cached(vk, l, now) {
                        Some(e) => e,
                        None => {
                            let e = decide();
                            g.tested_down(vk, l, e, now);
                            e
                        }
                    };
                    said = Some(e);
                }
                if said == Some(true) {
                    let e = g.key_down_cached(vk, l, now).unwrap_or_else(&mut decide);
                    g.finished_down(vk, e);
                    e
                } else {
                    false
                }
            }
            Host::ActOnly => {
                let e = g.key_down_cached(vk, l, now).unwrap_or_else(&mut decide);
                g.finished_down(vk, e);
                e
            }
        };
        assert_eq!(decided, 1, "{host:?}：同一次按键只判断一次");
        if !eaten {
            app.downs.push(vk);
        }
        eaten
    }

    fn release(g: &mut KeyGate, host: Host, app: &mut App, vk: u32, now: Instant) -> bool {
        let eaten = match host {
            Host::TestThenAct | Host::TestTwice => {
                let mut said = false;
                for _ in 0..if matches!(host, Host::TestTwice) { 2 } else { 1 } {
                    said = match g.up_cached(vk, now, false) {
                        Some(e) => e,
                        None => {
                            let e = g.decide_up(vk);
                            g.tested_up(vk, e, now);
                            e
                        }
                    };
                }
                if said {
                    g.up_cached(vk, now, true).unwrap_or_else(|| g.decide_up(vk))
                } else {
                    false
                }
            }
            Host::ActOnly => g.up_cached(vk, now, true).unwrap_or_else(|| g.decide_up(vk)),
        };
        if !eaten {
            app.ups.push(vk);
        }
        eaten
    }

    const HOSTS: [Host; 3] = [Host::TestThenAct, Host::ActOnly, Host::TestTwice];

    #[test]
    fn eaten_key_has_its_release_eaten_too() {
        for host in HOSTS {
            let (mut g, mut app, t) = (KeyGate::default(), App::default(), Instant::now());
            assert!(press(&mut g, host, &mut app, SPACE, lp(0x39, false), true, t));
            assert!(release(&mut g, host, &mut app, SPACE, t), "{host:?}");
            assert!(app.downs.is_empty() && app.ups.is_empty());
            assert!(g.held().is_empty());
        }
    }

    #[test]
    fn passed_key_always_gets_its_release() {
        for host in HOSTS {
            let (mut g, mut app, t) = (KeyGate::default(), App::default(), Instant::now());
            for vk in [SPACE, RIGHT, LEFT] {
                assert!(!press(&mut g, host, &mut app, vk, lp(0x4D, false), false, t));
                assert!(!release(&mut g, host, &mut app, vk, t), "{host:?}");
            }
            assert_eq!(app.downs, app.ups);
        }
    }

    /// 打拼音时按住 → 挪高亮（系统重复发了很多次按下），然后松开。
    /// 以前每次重复都记一笔、松开只消掉一笔，剩下的会吃掉以后看视频时 → 的松开（B 站就开了倍速）。
    #[test]
    fn holding_a_key_while_typing_leaves_nothing_behind() {
        for host in HOSTS {
            let (mut g, mut app, t) = (KeyGate::default(), App::default(), Instant::now());
            press(&mut g, host, &mut app, RIGHT, lp(0x4D, false), true, t);
            for i in 1..30 {
                press(&mut g, host, &mut app, RIGHT, lp(0x4D, true), true, t + Duration::from_millis(30 * i));
            }
            release(&mut g, host, &mut app, RIGHT, t + Duration::from_secs(1));
            assert!(g.held().is_empty(), "{host:?}");
            // 之后看视频：→ 的按下和松开都交给网页
            let later = t + Duration::from_secs(10);
            for _ in 0..5 {
                assert!(!press(&mut g, host, &mut app, RIGHT, lp(0x4D, false), false, later));
                assert!(!release(&mut g, host, &mut app, RIGHT, later), "{host:?}");
            }
            assert_eq!(app.downs.len(), 5);
            assert_eq!(app.ups.len(), 5);
        }
    }

    /// 被吃掉的按下没等到松开（中途切了窗口、换了输入框），下一次交给程序的按下要把账清掉。
    #[test]
    fn missed_release_does_not_eat_a_later_one() {
        for host in HOSTS {
            let (mut g, mut app, t) = (KeyGate::default(), App::default(), Instant::now());
            press(&mut g, host, &mut app, SPACE, lp(0x39, false), true, t);
            // 松开没来；过一会儿看视频按空格
            let later = t + Duration::from_secs(5);
            assert!(!press(&mut g, host, &mut app, SPACE, lp(0x39, false), false, later));
            assert!(!release(&mut g, host, &mut app, SPACE, later), "{host:?}");
            assert_eq!(app.downs, [SPACE]);
            assert_eq!(app.ups, [SPACE]);
        }
    }

    /// 程序不问、直接要"松开"：没吃过按下的键一律不吃（以前这里无条件吃掉所有松开）。
    #[test]
    fn release_without_asking_is_not_eaten() {
        let mut g = KeyGate::default();
        let t = Instant::now();
        for vk in [SPACE, RIGHT, LEFT, A] {
            assert_eq!(g.up_cached(vk, t, true), None);
            assert!(!g.decide_up(vk));
        }
    }

    /// 说了要、可是程序没来真正处理：松开交给程序（宁可多给一个松开，也不能少给）。
    #[test]
    fn asked_but_never_handled_does_not_eat_release() {
        let (mut g, t) = (KeyGate::default(), Instant::now());
        assert_eq!(g.test_down_cached(A, lp(0x1E, false), t), None);
        g.tested_down(A, lp(0x1E, false), true, t);
        assert!(!g.decide_up(A));
    }

    /// 问过的决定过一会儿就作废，不会套用到下一次按键上。
    #[test]
    fn cached_decision_expires() {
        let (mut g, t) = (KeyGate::default(), Instant::now());
        g.tested_down(A, lp(0x1E, false), true, t);
        assert_eq!(g.test_down_cached(A, lp(0x1E, false), t + Duration::from_secs(2)), None);
        g.tested_down(A, lp(0x1E, false), true, t);
        // 换了一个键：作废
        assert_eq!(g.key_down_cached(SPACE, lp(0x39, false), t), None);
        assert_eq!(g.key_down_cached(A, lp(0x1E, false), t), None);
    }

    #[test]
    fn many_keys_at_once_each_paired() {
        for host in HOSTS {
            let (mut g, mut app, t) = (KeyGate::default(), App::default(), Instant::now());
            // 打拼音时按下 A（吃掉），按着 A 又按了 → 和空格（这时都交给程序）
            press(&mut g, host, &mut app, A, lp(0x1E, false), true, t);
            press(&mut g, host, &mut app, RIGHT, lp(0x4D, false), false, t);
            press(&mut g, host, &mut app, SPACE, lp(0x39, false), false, t);
            release(&mut g, host, &mut app, SPACE, t);
            release(&mut g, host, &mut app, RIGHT, t);
            assert!(release(&mut g, host, &mut app, A, t), "{host:?}");
            assert_eq!(app.downs, [RIGHT, SPACE]);
            assert_eq!(app.ups, [SPACE, RIGHT]);
        }
    }

    #[test]
    fn system_keys_are_never_intercepted() {
        // F1–F12、F24、音量、播放暂停、截屏、菜单键、模拟输入
        for vk in [0x70, 0x7B, 0x87, 0xAD, 0xAE, 0xAF, 0xB3, 0x2C, 0x5D, 0xE7] {
            assert!(never_intercept(vk), "{vk:#x}");
        }
        // 字母、数字、空格、方向键、退格、回车、Esc、符号由输入法判断
        for vk in [0x41, 0x5A, 0x30, 0x39, 0x20, 0x25, 0x28, 0x08, 0x0D, 0x1B, 0xBA, 0xBD, 0xC0] {
            assert!(!never_intercept(vk), "{vk:#x}");
        }
    }

    #[test]
    fn only_non_text_keys_have_log_names() {
        assert_eq!(key_label(0x20), Some("空格"));
        assert_eq!(key_label(0x27), Some("→"));
        for vk in 0x30..=0x5A {
            assert_eq!(key_label(vk), None, "{vk:#x}");
        }
    }
}
