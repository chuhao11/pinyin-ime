//! 按键规则：把一个个按键变成"上屏什么字、候选窗显示什么"。
//!
//! 这部分和平台无关，Windows 外壳只负责把按键转给这里、把结果显示出来。
//! 规则见 docs/方案.md 的按键表。

use std::time::{SystemTime, UNIX_EPOCH};

use crate::dict::Dict;
use crate::lattice::{analyze, CandKind, Candidate};
use crate::pinyin::skip_apos;
use crate::userdb::UserDb;

pub const GRID_COLS: usize = 5;
pub const GRID_SIZE: usize = 25;

/// 按键。`Char` 是这个键实际打出的字符（已经考虑了 Shift），比如 Shift+1 是 `Char('!')`。
/// 单按 Shift 不需要传进来（它不做任何事）。
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
}

#[derive(Clone, Copy, Debug, Default)]
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
    fn eaten() -> Self {
        Response { consumed: true, commit: None }
    }
    fn pass() -> Self {
        Response { consumed: false, commit: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 候选条
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
}

#[derive(Clone, Debug)]
pub struct Config {
    /// 候选条最多占多宽（单字算 2，字母算 1，每个候选另加 4）
    pub row_width: usize,
    pub row_min: usize,
    pub row_max: usize,
    /// 输入最多多少个字母
    pub max_input: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config { row_width: 50, row_min: 4, row_max: 8, max_input: 64 }
    }
}

#[derive(Clone, Debug)]
struct Selected {
    /// 这次选择从输入的哪里开始
    start: usize,
    text: String,
    code: Vec<u16>,
    learnable: bool,
}

pub struct Ime {
    dict: Dict,
    user: UserDb,
    clock: Box<dyn Fn() -> i64 + Send>,
    pub config: Config,
    input: String,
    consumed: usize,
    selected: Vec<Selected>,
    cands: Vec<Candidate>,
    row_len: usize,
    hl: usize,
    mode: Mode,
}

fn system_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

impl Ime {
    pub fn new(dict: Dict, user: UserDb) -> Self {
        Ime {
            dict,
            user,
            clock: Box::new(system_now),
            config: Config::default(),
            input: String::new(),
            consumed: 0,
            selected: Vec::new(),
            cands: Vec::new(),
            row_len: 0,
            hl: 0,
            mode: Mode::Row,
        }
    }

    /// 换一个时钟（测试用）。
    pub fn set_clock(&mut self, f: impl Fn() -> i64 + Send + 'static) {
        self.clock = Box::new(f);
    }

    pub fn dict(&self) -> &Dict {
        &self.dict
    }

    pub fn user(&self) -> &UserDb {
        &self.user
    }

    pub fn user_mut(&mut self) -> &mut UserDb {
        &mut self.user
    }

    pub fn is_composing(&self) -> bool {
        !self.input.is_empty()
    }

    pub fn candidates(&self) -> &[Candidate] {
        &self.cands
    }

    /// 处理一个按键。
    pub fn process_key(&mut self, key: Key, mods: Mods) -> Response {
        let key = if key == Key::Char(' ') { Key::Space } else { key };
        if mods.ctrl || mods.alt {
            return Response::pass();
        }
        if !self.is_composing() {
            if let Key::Char(c) = key {
                if c.is_ascii_alphabetic() && !mods.caps_lock {
                    self.push_char(c);
                    return Response::eaten();
                }
            }
            return Response::pass();
        }
        match self.mode {
            Mode::Row => self.key_row(key),
            Mode::Grid => self.key_grid(key),
        }
    }

    fn key_row(&mut self, key: Key) -> Response {
        match key {
            Key::Char(c) if c.is_ascii_alphabetic() => {
                self.push_char(c);
                Response::eaten()
            }
            Key::Char('\'') => {
                if !self.input.ends_with('\'') {
                    self.push_char('\'');
                }
                Response::eaten()
            }
            Key::Char(d @ '1'..='9') => {
                let i = d as usize - '1' as usize;
                if i < self.row_len {
                    return self.select(i);
                }
                Response::eaten()
            }
            Key::Char('0') => Response::eaten(),
            Key::Char('-') => {
                if !self.cands.is_empty() {
                    self.mode = Mode::Grid;
                    // 高亮停在候选条里没显示过的第一个
                    self.hl = self.row_len.min(self.cands.len() - 1);
                }
                Response::eaten()
            }
            Key::Char(c) => self.commit_with(Some(c)),
            Key::Space => {
                if self.cands.is_empty() {
                    self.commit_with(None)
                } else {
                    self.select(self.hl)
                }
            }
            Key::Enter => self.commit_raw(),
            Key::Escape => {
                self.reset();
                Response::eaten()
            }
            Key::Backspace => {
                self.backspace();
                Response::eaten()
            }
            Key::Left => {
                self.hl = self.hl.saturating_sub(1);
                Response::eaten()
            }
            Key::Right => {
                if self.hl + 1 < self.row_len {
                    self.hl += 1;
                }
                Response::eaten()
            }
            Key::Up | Key::Down | Key::Other => Response::eaten(),
        }
    }

    fn key_grid(&mut self, key: Key) -> Response {
        let len = self.cands.len();
        match key {
            Key::Char(c) if c.is_ascii_alphabetic() || c == '\'' => {
                self.mode = Mode::Row;
                self.key_row(key)
            }
            Key::Char(d @ '1'..='5') => {
                let row_start = self.hl / GRID_COLS * GRID_COLS;
                let i = row_start + (d as usize - '1' as usize);
                if i < len {
                    return self.select(i);
                }
                Response::eaten()
            }
            Key::Char(c) if c.is_ascii_digit() => Response::eaten(),
            Key::Char('-') | Key::Escape => {
                self.mode = Mode::Row;
                self.hl = 0;
                Response::eaten()
            }
            Key::Char(c) => self.commit_with(Some(c)),
            Key::Space => self.select(self.hl),
            Key::Enter => self.commit_raw(),
            Key::Backspace => {
                self.backspace();
                Response::eaten()
            }
            Key::Left => {
                self.hl = self.hl.saturating_sub(1);
                Response::eaten()
            }
            Key::Right => {
                if self.hl + 1 < len {
                    self.hl += 1;
                }
                Response::eaten()
            }
            Key::Up => {
                if self.hl >= GRID_COLS {
                    self.hl -= GRID_COLS;
                }
                Response::eaten()
            }
            Key::Down => {
                if self.hl + GRID_COLS < len {
                    self.hl += GRID_COLS;
                } else if self.hl / GRID_COLS < (len - 1) / GRID_COLS {
                    // 下一行不满，跳到最后一个
                    self.hl = len - 1;
                }
                Response::eaten()
            }
            Key::Other => Response::eaten(),
        }
    }

    fn push_char(&mut self, c: char) {
        if self.input.len() < self.config.max_input {
            self.input.push(c);
            self.refresh();
        }
    }

    fn remaining(&self) -> &str {
        &self.input[self.consumed..]
    }

    fn refresh(&mut self) {
        let now = (self.clock)();
        let rem = &self.input[self.consumed..];
        self.cands = if rem.is_empty() { Vec::new() } else { analyze(&self.dict, &self.user, rem, now) };
        self.row_len = self.compute_row_len();
        self.hl = 0;
        self.mode = Mode::Row;
    }

    fn compute_row_len(&self) -> usize {
        let cfg = &self.config;
        let (mut width, mut k) = (0usize, 0usize);
        for c in &self.cands {
            if k >= cfg.row_max || (k > 0 && c.poor) {
                break;
            }
            let cost = 4 + display_width(&c.text);
            if k >= cfg.row_min && width + cost > cfg.row_width {
                break;
            }
            width += cost;
            k += 1;
        }
        k
    }

    fn select(&mut self, i: usize) -> Response {
        let Some(c) = self.cands.get(i).cloned() else { return Response::eaten() };
        self.take(c);
        if self.consumed >= self.input.len() {
            self.finish(None)
        } else {
            self.refresh();
            Response::eaten()
        }
    }

    /// 把一个候选放进"已选"，往后移动输入位置。
    fn take(&mut self, c: Candidate) {
        let start = self.consumed;
        if c.kind == CandKind::Raw {
            self.selected.push(Selected { start, text: c.text, code: Vec::new(), learnable: false });
            self.consumed = self.input.len();
        } else {
            self.selected.push(Selected { start, text: c.text, code: c.code, learnable: c.learnable });
            self.consumed = skip_apos(self.input.as_bytes(), start + c.consumed).min(self.input.len());
        }
    }

    /// 上屏全部已选的字，记住用过的词，然后清空。
    fn finish(&mut self, extra: Option<char>) -> Response {
        let now = (self.clock)();
        let mut text: String = self.selected.iter().map(|s| s.text.as_str()).collect();
        for s in &self.selected {
            if s.learnable && !s.code.is_empty() {
                self.user.learn(&self.dict, &s.code, &s.text, now);
            }
        }
        // 分几次选出来的，合起来存成一个新词（比如人名）
        if self.selected.len() >= 2 && self.selected.iter().all(|s| s.learnable && !s.code.is_empty()) {
            let n = text.chars().count();
            if (2..=8).contains(&n) {
                let code: Vec<u16> = self.selected.iter().flat_map(|s| s.code.iter().copied()).collect();
                self.user.learn(&self.dict, &code, &text, now);
            }
        }
        if let Some(c) = extra {
            text.push(c);
        }
        self.reset();
        Response { consumed: true, commit: Some(text) }
    }

    /// 按了符号（或者没有候选时按空格）：先上屏高亮的候选，再输出符号。
    fn commit_with(&mut self, sym: Option<char>) -> Response {
        let rem = self.remaining().len();
        if rem > 0 {
            let covers = |c: &Candidate| c.consumed >= rem;
            let pick = match self.cands.get(self.hl) {
                Some(c) if covers(c) => Some(c.clone()),
                _ => self.cands.first().filter(|c| covers(c)).cloned(),
            };
            match pick {
                Some(c) => self.take(c),
                None => {
                    let raw = self.remaining().to_string();
                    let start = self.consumed;
                    self.selected.push(Selected { start, text: raw, code: Vec::new(), learnable: false });
                    self.consumed = self.input.len();
                }
            }
        }
        self.finish(sym)
    }

    /// 回车：已选的字 + 剩下的字母原样上屏。
    fn commit_raw(&mut self) -> Response {
        let mut text: String = self.selected.iter().map(|s| s.text.as_str()).collect();
        text.push_str(self.remaining());
        self.reset();
        Response { consumed: true, commit: Some(text) }
    }

    fn backspace(&mut self) {
        if self.input.len() > self.consumed {
            self.input.pop();
        } else if let Some(s) = self.selected.pop() {
            self.consumed = s.start;
        }
        if self.input.is_empty() {
            self.reset();
        } else {
            self.refresh();
        }
    }

    pub fn reset(&mut self) {
        self.input.clear();
        self.consumed = 0;
        self.selected.clear();
        self.cands.clear();
        self.cands.shrink_to(64);
        self.row_len = 0;
        self.hl = 0;
        self.mode = Mode::Row;
    }

    pub fn view(&self) -> View {
        let mut preedit: String = self.selected.iter().map(|s| s.text.as_str()).collect();
        preedit.push_str(self.remaining());
        let total = self.cands.len();
        let (items, highlight, page, pages) = match self.mode {
            Mode::Row => (
                self.cands[..self.row_len].iter().map(|c| c.text.clone()).collect(),
                self.hl.min(self.row_len.saturating_sub(1)),
                0,
                1,
            ),
            Mode::Grid => {
                let page = self.hl / GRID_SIZE;
                let start = page * GRID_SIZE;
                let end = (start + GRID_SIZE).min(total);
                (
                    self.cands[start..end].iter().map(|c| c.text.clone()).collect(),
                    self.hl - start,
                    page,
                    total.div_ceil(GRID_SIZE),
                )
            }
        };
        View { composing: self.is_composing(), preedit, mode: self.mode, items, highlight, page, pages, total }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictBuilder;

    fn ime() -> Ime {
        let syl = ["zhang", "zi", "han", "shi", "qing", "ni", "hao", "xian", "xi", "an", "a"];
        let mut b = DictBuilder::new(syl.map(String::from));
        let mut add = |py: &str, t: &str, s: f32| {
            let c = b.code(py).unwrap();
            b.add(&c, t, s);
        };
        add("zhang", "张", -6.0);
        add("zhang", "章", -6.5);
        add("zi", "子", -5.5);
        add("zi", "紫", -7.5);
        add("han", "涵", -9.0);
        add("han", "含", -7.0);
        add("zi han", "紫涵", -12.0);
        add("shi", "是", -3.0);
        add("shi", "事", -5.0);
        add("shi qing", "事情", -7.0);
        add("ni", "你", -4.0);
        add("hao", "好", -4.5);
        add("ni hao", "你好", -6.0);
        add("a", "啊", -5.0);
        // 让 xian 有 30 个以上的候选，测试矩阵翻页
        for i in 0..40 {
            add("xian", &format!("先{i}"), -6.0 - i as f32 * 0.01);
        }
        add("xi an", "西安", -9.0);
        let d = Dict::from_bytes(b.build()).unwrap();
        let mut ime = Ime::new(d, UserDb::in_memory());
        ime.set_clock(|| 1_000_000);
        ime
    }

    fn typ(ime: &mut Ime, s: &str) -> String {
        let mut out = String::new();
        for c in s.chars() {
            let r = ime.process_key(Key::Char(c), Mods::default());
            if let Some(t) = r.commit {
                out.push_str(&t);
            } else if !r.consumed {
                out.push(c);
            }
        }
        out
    }

    fn key(ime: &mut Ime, k: Key) -> Response {
        ime.process_key(k, Mods::default())
    }

    #[test]
    fn symbols_pass_through_when_idle() {
        let mut m = ime();
        for c in ",.!@#$%^&*()-=_+[]{};:\"<>?/\\|`~1234567890".chars() {
            assert_eq!(key(&mut m, Key::Char(c)), Response::pass(), "{c}");
        }
        assert_eq!(key(&mut m, Key::Space), Response::pass());
        assert_eq!(key(&mut m, Key::Enter), Response::pass());
        assert!(!m.is_composing());
    }

    #[test]
    fn symbol_commits_candidate_then_symbol() {
        let mut m = ime();
        assert_eq!(typ(&mut m, "nihao,"), "你好,");
        assert_eq!(typ(&mut m, "nihao="), "你好=");
        assert_eq!(typ(&mut m, "nihao("), "你好(");
        assert_eq!(typ(&mut m, "shi^"), "是^");
    }

    #[test]
    fn space_and_digits_select() {
        let mut m = ime();
        assert_eq!(typ(&mut m, "shi "), "是");
        assert_eq!(typ(&mut m, "shi2"), "事");
        // 数字超出候选条：什么都不做
        typ(&mut m, "shi");
        assert_eq!(key(&mut m, Key::Char('9')), Response::eaten());
        assert!(m.is_composing());
    }

    #[test]
    fn enter_commits_letters() {
        let mut m = ime();
        typ(&mut m, "nihao");
        assert_eq!(key(&mut m, Key::Enter).commit.as_deref(), Some("nihao"));
        assert!(!m.is_composing());
    }

    #[test]
    fn arrows_move_highlight_in_row() {
        let mut m = ime();
        typ(&mut m, "shi");
        key(&mut m, Key::Right);
        assert_eq!(m.view().highlight, 1);
        assert_eq!(key(&mut m, Key::Space).commit.as_deref(), Some("事"));
        // 上下键在候选条里不做事，但不能漏给程序
        typ(&mut m, "shi");
        assert_eq!(key(&mut m, Key::Down), Response::eaten());
    }

    #[test]
    fn grid_opens_with_minus_and_navigates() {
        let mut m = ime();
        typ(&mut m, "xian");
        let row_len = m.view().items.len();
        assert!((4..=8).contains(&row_len));
        key(&mut m, Key::Char('-'));
        let v = m.view();
        assert_eq!(v.mode, Mode::Grid);
        assert_eq!(v.items.len(), 25);
        assert_eq!(v.highlight, row_len, "高亮停在候选条后面第一个");
        // 往下走到第二页：在最后一行再按 ↓ 换下一批
        let mut downs = 0;
        while m.view().page == 0 {
            key(&mut m, Key::Down);
            downs += 1;
            assert!(downs <= 5);
        }
        assert_eq!(m.view().items.len(), 41 - 25);
        key(&mut m, Key::Up);
        assert_eq!(m.view().page, 0);
        // 数字选高亮那一行的第几个
        let v = m.view();
        let row = v.highlight / 5 * 5;
        let want = v.items[row + 2].clone();
        assert_eq!(key(&mut m, Key::Char('3')).commit, Some(want));
    }

    #[test]
    fn minus_closes_grid_and_equal_is_plain_symbol() {
        let mut m = ime();
        typ(&mut m, "xian");
        key(&mut m, Key::Char('-'));
        key(&mut m, Key::Char('-'));
        assert_eq!(m.view().mode, Mode::Row);
        let first = m.view().items[0].clone();
        assert_eq!(key(&mut m, Key::Char('=')).commit, Some(format!("{first}=")));
    }

    #[test]
    fn partial_selection_builds_and_learns_a_name() {
        let mut m = ime();
        typ(&mut m, "zhangzihan");
        // 一开始没有"张子涵"这个词
        assert!(!m.view().items.contains(&"张子涵".to_string()));
        // 选"张"，再选"子"，再选"涵"
        let pick = |m: &mut Ime, t: &str| {
            let i = m.candidates().iter().position(|c| c.text == t).unwrap();
            assert!(i < 8, "{t} 应该在候选条里");
            m.process_key(Key::Char(char::from(b'1' + i as u8)), Mods::default())
        };
        assert_eq!(pick(&mut m, "张").commit, None);
        assert_eq!(m.view().preedit, "张zihan");
        assert_eq!(pick(&mut m, "子").commit, None);
        assert_eq!(pick(&mut m, "涵").commit.as_deref(), Some("张子涵"));
        // 下次直接排第一
        typ(&mut m, "zhangzihan");
        assert_eq!(m.view().items[0], "张子涵");
    }

    #[test]
    fn single_chars_do_not_reorder() {
        let mut m = ime();
        for _ in 0..5 {
            assert_eq!(typ(&mut m, "shi2"), "事");
        }
        typ(&mut m, "shi");
        assert_eq!(m.view().items[0], "是");
    }

    #[test]
    fn backspace_undoes_selection() {
        let mut m = ime();
        typ(&mut m, "nihao");
        let i = m.candidates().iter().position(|c| c.text == "你").unwrap();
        key(&mut m, Key::Char(char::from(b'1' + i as u8)));
        assert_eq!(m.view().preedit, "你hao");
        for _ in 0..3 {
            key(&mut m, Key::Backspace);
        }
        assert_eq!(m.view().preedit, "你");
        key(&mut m, Key::Backspace);
        assert_eq!(m.view().preedit, "ni");
        key(&mut m, Key::Backspace);
        key(&mut m, Key::Backspace);
        assert!(!m.is_composing());
    }

    #[test]
    fn uppercase_offers_raw_text_first() {
        let mut m = ime();
        typ(&mut m, "Nihao");
        assert_eq!(m.view().items[0], "Nihao");
        assert_eq!(key(&mut m, Key::Space).commit.as_deref(), Some("Nihao"));
    }

    #[test]
    fn caps_lock_and_ctrl_pass_through() {
        let mut m = ime();
        let caps = Mods { caps_lock: true, ..Mods::default() };
        assert_eq!(m.process_key(Key::Char('A'), caps), Response::pass());
        let ctrl = Mods { ctrl: true, ..Mods::default() };
        assert_eq!(m.process_key(Key::Char('c'), ctrl), Response::pass());
    }

    #[test]
    fn escape_clears() {
        let mut m = ime();
        typ(&mut m, "nihao");
        key(&mut m, Key::Escape);
        assert!(!m.is_composing());
    }

    #[test]
    fn apostrophe_only_inside_composition() {
        let mut m = ime();
        assert_eq!(key(&mut m, Key::Char('\'')), Response::pass());
        typ(&mut m, "xi'an");
        assert_eq!(m.view().items[0], "西安");
    }
}
