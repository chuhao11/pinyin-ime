//! "单独按一下 Shift"的判断（和平台无关，在 Mac 上也能测）。
//!
//! 算一次单按：按下到松开不超过 0.8 秒，中间没按别的键，也没点鼠标。
//! Shift+字母（大写）、Shift+点鼠标选文字、按住 Shift 很久、Ctrl+Shift、Alt+Shift 都不算。
//!
//! 同一次松开可能有两条路来报：系统的"保留键"检测，和我们自己的按键接口。**只认每一次真实的按键，
//! 不按时间挡**：每次按下都是新的一次，连按多快都每次都切换（打一个英文字母再切回中文：Shift、a、Shift）。
//! 两条路报的是不是同一次松开：看我们有没有见过这次按下；没见过的话，看是不是几十毫秒内刚处理过一次松开
//! （同一次松开的两次报告前后只差几毫秒，人手连按两下 Shift 不可能这么快）。

use std::time::{Duration, Instant};

/// 按下到松开最多这么久才算"单按"
pub const TAP: Duration = Duration::from_millis(800);

/// 同一次松开的两次报告之间最多隔这么久（实际只差几毫秒）
pub const SAME_RELEASE: Duration = Duration::from_millis(80);

#[derive(Default, Debug)]
pub struct ShiftTap {
    /// 这次按下的时间（按键接口看到了按下才有）
    down_at: Option<Instant>,
    /// 按着的时候按了别的键或者点了鼠标
    spoiled: bool,
    /// 按键接口上一次处理松开的时间（不管算不算单按）
    last_up: Option<Instant>,
}

impl ShiftTap {
    /// Shift 按下。`repeat`：按住不放时系统重复发的"按下"；`mouse_down`：这时鼠标键按着。
    pub fn down(&mut self, repeat: bool, mouse_down: bool, now: Instant) {
        if repeat && self.down_at.is_some() {
            return;
        }
        // 新的一次按下：不管上一次的松开有没有收到（比如按着 Shift 切了窗口），都重新开始
        self.down_at = Some(now);
        self.spoiled = mouse_down;
    }

    /// 按着 Shift 时按了别的键、点了鼠标：这次不算单按。
    pub fn spoil(&mut self) {
        if self.down_at.is_some() {
            self.spoiled = true;
        }
    }

    /// 按着 Shift 吗（看到了按下、还没处理松开）
    pub fn held(&self) -> bool {
        self.down_at.is_some()
    }

    fn verdict(&mut self, t: Instant, mouse_down: bool, now: Instant) -> bool {
        let tap = !self.spoiled && !mouse_down && now.saturating_duration_since(t) <= TAP;
        self.spoiled = false;
        tap
    }

    /// 按键接口报 Shift 松开：要不要切换中英。
    pub fn key_up(&mut self, mouse_down: bool, now: Instant) -> bool {
        // 没见过这次按下（系统的保留键已经处理了，或者按下没经过我们）：交给那条路
        let Some(t) = self.down_at.take() else { return false };
        self.last_up = Some(now);
        self.verdict(t, mouse_down, now)
    }

    /// 系统的保留键检测报"单独按了一下 Shift"：要不要切换中英。
    pub fn preserved(&mut self, mouse_down: bool, now: Instant) -> bool {
        match self.down_at.take() {
            // 我们也看到了这次按下：按我们的判断（我们还知道中间有没有点鼠标）。
            // 之后按键接口再报这次松开，就不再算了
            Some(t) => {
                self.last_up = Some(now);
                self.verdict(t, mouse_down, now)
            }
            None => {
                // 同一次松开刚处理过（按键接口报过，或者系统的两种登记各报了一次）：已经判断过了
                if self.last_up.is_some_and(|u| now.saturating_duration_since(u) < SAME_RELEASE) {
                    return false;
                }
                self.last_up = Some(now);
                // 这个程序没把 Shift 交给按键接口：相信系统的判断（鼠标按着就是在选文字，不算）
                !mouse_down
            }
        }
    }

    /// 停用、换了地方：忘掉按着的状态。
    pub fn reset(&mut self) {
        self.down_at = None;
        self.spoiled = false;
    }

    /// 给诊断日志用
    pub fn describe(&self, now: Instant) -> String {
        format!(
            "见过按下={} 按了别的键或点了鼠标={} 按了{}毫秒",
            self.down_at.is_some(),
            self.spoiled,
            self.down_at.map(|t| now.saturating_duration_since(t).as_millis()).unwrap_or(0)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// 一次干净的单按：t 时按下，t+hold 时松开。`via`：哪几条路报松开（按顺序）。
    fn tap(s: &mut ShiftTap, t: Instant, hold: u64, via: &[&str]) -> u32 {
        s.down(false, false, t);
        let up = t + ms(hold);
        let mut toggles = 0;
        for (i, v) in via.iter().enumerate() {
            let now = up + ms(i as u64); // 同一次松开的几次报告只差一两毫秒
            let r = match *v {
                "key" => s.key_up(false, now),
                "sys" => s.preserved(false, now),
                _ => unreachable!(),
            };
            toggles += r as u32;
        }
        toggles
    }

    /// 只有系统保留键报（这个程序不把 Shift 交给按键接口）
    fn sys_only_tap(s: &mut ShiftTap, at: Instant) -> u32 {
        s.preserved(false, at) as u32
    }

    #[test]
    fn every_quick_tap_counts_however_fast() {
        // 用户的例子：Shift 切英文、打一个字母、马上 Shift 切回中文，前后不到 0.2 秒
        for via in [&["key", "sys"][..], &["sys", "key"], &["key"], &["sys"], &["key", "key", "sys"]] {
            let mut s = ShiftTap::default();
            let t0 = Instant::now();
            assert_eq!(tap(&mut s, t0, 60, via), 1, "{via:?}");
            // 中间打了一个字母（Shift 松开以后，不算按着时按别的键）
            s.spoil();
            assert_eq!(tap(&mut s, t0 + ms(150), 60, via), 1, "{via:?}");
            // 再快一点：连按两下 Shift，间隔 100 毫秒
            assert_eq!(tap(&mut s, t0 + ms(300), 40, via), 1, "{via:?}");
            assert_eq!(tap(&mut s, t0 + ms(400), 40, via), 1, "{via:?}");
        }
    }

    #[test]
    fn one_release_reported_several_times_toggles_once() {
        for via in [
            &["key", "sys"][..],
            &["sys", "key"],
            &["key", "key"],
            &["sys", "key", "key"],
            // 系统的两种登记都报了
            &["sys", "sys"],
            &["key", "sys", "sys"],
            &["sys", "sys", "key"],
        ] {
            let mut s = ShiftTap::default();
            assert_eq!(tap(&mut s, Instant::now(), 100, via), 1, "{via:?}");
        }
    }

    #[test]
    fn programs_that_only_tell_the_system_still_toggle_fast() {
        let mut s = ShiftTap::default();
        let t0 = Instant::now();
        assert_eq!(sys_only_tap(&mut s, t0), 1);
        // 同一次松开，系统的两种登记各报一次
        assert_eq!(sys_only_tap(&mut s, t0 + ms(1)), 0);
        assert_eq!(sys_only_tap(&mut s, t0 + ms(120)), 1);
        assert_eq!(sys_only_tap(&mut s, t0 + ms(240)), 1);
    }

    #[test]
    fn shift_with_another_key_or_mouse_is_not_a_tap() {
        let t0 = Instant::now();
        // Shift+字母（打大写）
        let mut s = ShiftTap::default();
        s.down(false, false, t0);
        s.spoil();
        assert!(!s.key_up(false, t0 + ms(100)));
        // 系统紧接着也报了：不算（我们已经判断过这次松开了）
        assert!(!s.preserved(false, t0 + ms(101)));

        // Shift+点鼠标选文字：系统先报
        let mut s = ShiftTap::default();
        s.down(false, false, t0);
        s.spoil();
        assert!(!s.preserved(false, t0 + ms(100)));
        assert!(!s.key_up(false, t0 + ms(101)));

        // 松开时鼠标还按着（拖着选文字）
        let mut s = ShiftTap::default();
        s.down(false, false, t0);
        assert!(!s.key_up(true, t0 + ms(100)));
        assert!(!s.preserved(true, t0 + ms(500)));
    }

    #[test]
    fn long_hold_is_not_a_tap() {
        let mut s = ShiftTap::default();
        let t0 = Instant::now();
        assert_eq!(tap(&mut s, t0, 900, &["key", "sys"]), 0);
        // 按住不放（系统连发"按下"）也只算一次按下
        s.down(false, false, t0 + ms(2000));
        for i in 1..10 {
            s.down(true, false, t0 + ms(2000 + i * 30));
        }
        assert!(s.key_up(false, t0 + ms(2400)));
    }

    #[test]
    fn missed_release_does_not_block_the_next_tap() {
        let mut s = ShiftTap::default();
        let t0 = Instant::now();
        // 按着 Shift 切走了窗口，没收到松开
        s.down(false, false, t0);
        // 回来再单按一次
        assert_eq!(tap(&mut s, t0 + ms(5000), 80, &["key", "sys"]), 1);
    }
}
