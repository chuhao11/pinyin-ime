//! 长时间打字的内存测试：连续模拟大量按键，内存不能越用越多。
//!
//! 用一个会计数的内存分配器统计"现在还占着的内存"，热身后记下基准，
//! 之后整个过程中的最高值不能比基准多出太多。

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use pinyin_engine::{Ime, Key, Mods, UserDb};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            LIVE.fetch_add(l.size() as isize, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            LIVE.fetch_add(new as isize - l.size() as isize, Ordering::Relaxed);
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

/// 简单的伪随机数（不用外部库）
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn memory_stays_flat_over_long_typing() {
    let Some(dict) = common::real_dict() else { return };
    let syllables: Vec<String> = dict.syllables().to_vec();
    let mut user = UserDb::in_memory();
    // 上限调小，让淘汰机制在测试里反复触发
    user.set_max_entries(300);
    let mut ime = Ime::new(dict, user);
    let mut clock = 1_800_000_000i64;
    let mut rng = Rng(0x9E3779B97F4A7C15);

    let total_keys: usize = if cfg!(debug_assertions) { 6_000 } else { 120_000 };
    let warmup = total_keys / 5;
    let mut keys = 0usize;
    let mut baseline: Option<isize> = None;
    let mut peak = 0isize;
    let mut commits = 0usize;

    while keys < total_keys {
        clock += 3;
        let t = clock;
        ime.set_clock(move || t);
        // 打 1–5 个音节
        let n = 1 + rng.below(5);
        for _ in 0..n {
            let s = &syllables[rng.below(syllables.len())];
            let s = if rng.below(4) == 0 { &s[..1] } else { s.as_str() };
            for c in s.chars() {
                ime.process_key(Key::Char(c), Mods::default());
                keys += 1;
            }
        }
        // 随机做点什么
        let action = rng.below(10);
        let mut seq: Vec<Key> = match action {
            0 => vec![Key::Space],
            1 => vec![Key::Char('2')],
            2 => vec![Key::Char('-'), Key::Down, Key::Right, Key::Space],
            3 => vec![Key::Char(',')],
            4 => vec![Key::Enter],
            5 => vec![Key::Backspace, Key::Backspace, Key::Space],
            6 => vec![Key::Escape],
            7 => vec![Key::Char('1'), Key::Char('1'), Key::Space],
            8 => vec![Key::Right, Key::Right, Key::Space],
            _ => vec![Key::Char('-'), Key::Char('3')],
        };
        seq.push(Key::Escape);
        for k in seq {
            if ime.process_key(k, Mods::default()).commit.is_some() {
                commits += 1;
            }
            keys += 1;
        }
        let live = LIVE.load(Ordering::Relaxed);
        if keys >= warmup {
            match baseline {
                None => baseline = Some(live),
                Some(_) => peak = peak.max(live),
            }
        }
    }
    let base = baseline.unwrap();
    let growth = peak - base;
    eprintln!(
        "按键 {keys}，上屏 {commits} 次，记住 {} 个词；热身后内存 {:.2} MB，最高 {:.2} MB，增长 {:.2} MB",
        ime.user().len(),
        base as f64 / 1048576.0,
        peak as f64 / 1048576.0,
        growth as f64 / 1048576.0
    );
    assert!(ime.user().len() <= 300);
    assert!(commits > 100);
    assert!(growth < 4 * 1024 * 1024, "内存增长了 {growth} 字节");
}
