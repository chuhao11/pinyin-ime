//! 按键规则：把一个个按键变成"上屏什么字、候选窗显示什么"。
//!
//! 这部分和平台无关，Windows 外壳只负责把按键转给这里、把结果显示出来。
//! 规则见 docs/方案.md 的按键表。
//!
//! - `Engine`：所有程序共用的东西（词库、语言模型、你的记忆、设置、中/英模式）
//! - `Session`：每个程序（每个输入框）自己的输入状态（正在打的拼音、候选、上文）
//! - `Ime`：一个 Engine 配一个 Session，单个程序用起来方便（试打工具、测试）

use std::time::{SystemTime, UNIX_EPOCH};

use crate::dict::Dict;
use crate::english::EnglishDict;
use crate::lattice::{analyze_with, CandKind, Candidate, Query};
use crate::lm::{Lm, NUM_TOKEN};
use crate::pinyin::{skip_apos, Fix};
use crate::userdb::UserDb;

/// 上文（刚上屏的词）多久以内还算数。
pub const CONTEXT_SECS: i64 = 30;
pub const GRID_COLS: usize = 5;
pub const GRID_SIZE: usize = 25;

pub use pinyin_types::{Key, Mode, Mods, Response, View};

#[derive(Clone, Debug)]
pub struct Config {
    /// 候选条最多占多宽（单字算 2，字母算 1，每个候选另加 4）
    pub row_width: usize,
    pub row_min: usize,
    pub row_max: usize,
    /// 输入最多多少个字母
    pub max_input: usize,
    /// 中文标点：打开时 ,.?! 等变成 ，。？！（默认关，全是英文符号）
    pub chinese_punct: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { row_width: 50, row_min: 4, row_max: 8, max_input: 64, chinese_punct: false }
    }
}

#[derive(Clone, Debug)]
struct Selected {
    /// 这次选择从输入的哪里开始
    start: usize,
    text: String,
    code: Vec<u16>,
    learnable: bool,
    fixes: Vec<Fix>,
    typed_code: Option<Vec<u16>>,
    /// 英文词（或原样的字母），上屏后记进英文词表
    english: bool,
    /// 最后一个词在语言模型里的编号
    last_lm: Option<u32>,
}

impl Selected {
    fn plain(start: usize, text: String) -> Self {
        Selected {
            start,
            text,
            code: Vec::new(),
            learnable: false,
            fixes: Vec::new(),
            typed_code: None,
            english: false,
            last_lm: None,
        }
    }
}

/// 一个程序（输入框）自己的输入状态。
#[derive(Default)]
pub struct Session {
    input: String,
    consumed: usize,
    selected: Vec<Selected>,
    cands: Vec<Candidate>,
    row_len: usize,
    hl: usize,
    mode: Mode,
    /// 上文：刚上屏的最后一个词和上屏时间。只在内存里，不保存。
    ctx: Option<(u32, i64)>,
    /// 中文引号是不是正开着（下一个是右引号）
    dq_open: bool,
    sq_open: bool,
}

impl Session {
    pub fn new() -> Self {
        Session::default()
    }

    pub fn is_composing(&self) -> bool {
        !self.input.is_empty()
    }

    pub fn candidates(&self) -> &[Candidate] {
        &self.cands
    }

    /// 忘掉上文（比如光标移走了）。
    pub fn clear_context(&mut self) {
        self.ctx = None;
    }

    /// 清空正在打的拼音。
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

    fn remaining(&self) -> &str {
        &self.input[self.consumed..]
    }
}

/// 所有程序共用的部分。
pub struct Engine {
    dict: Dict,
    en: Option<EnglishDict>,
    lm: Option<Lm>,
    user: UserDb,
    clock: Box<dyn Fn() -> i64 + Send>,
    pub config: Config,
    /// 纯英文输入（单按 Shift 切换，所有程序一起）
    english_mode: bool,
}

fn system_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// 有中文写法的标点。中文标点开着时，这些键全部变成中文标点，没有例外；关着时全部是英文。
const ZH_PUNCT_KEYS: &str = ",.?!:;()[]{}<>\\^_$~`\"'";

/// 中文标点（开关打开时用）。
fn zh_punct(c: char, s: &mut Session) -> Option<&'static str> {
    Some(match c {
        ',' => "，",
        '.' => "。",
        '?' => "？",
        '!' => "！",
        ':' => "：",
        ';' => "；",
        '(' => "（",
        ')' => "）",
        '[' => "【",
        ']' => "】",
        '{' => "｛",
        '}' => "｝",
        '<' => "《",
        '>' => "》",
        '\\' => "、",
        '^' => "……",
        '_' => "——",
        '$' => "￥",
        '~' => "～",
        '`' => "·",
        '"' => {
            s.dq_open = !s.dq_open;
            if s.dq_open {
                "“"
            } else {
                "”"
            }
        }
        '\'' => {
            s.sq_open = !s.sq_open;
            if s.sq_open {
                "‘"
            } else {
                "’"
            }
        }
        _ => return None,
    })
}

impl Engine {
    pub fn new(dict: Dict, user: UserDb) -> Self {
        Engine {
            dict,
            en: None,
            lm: None,
            user,
            clock: Box::new(system_now),
            config: Config::default(),
            english_mode: false,
        }
    }

    /// 装上英文词表。
    pub fn set_english(&mut self, en: EnglishDict) {
        self.en = Some(en);
    }

    /// 装上（或换掉）语言模型。
    pub fn set_lm(&mut self, lm: Lm) {
        self.lm = Some(lm);
    }

    pub fn lm(&self) -> Option<&Lm> {
        self.lm.as_ref()
    }

    pub fn english_mode(&self) -> bool {
        self.english_mode
    }

    pub fn set_english_mode(&mut self, on: bool) {
        self.english_mode = on;
    }

    /// 换一个时钟（测试用）。
    pub fn set_clock(&mut self, f: impl Fn() -> i64 + Send + 'static) {
        self.clock = Box::new(f);
    }

    pub fn now(&self) -> i64 {
        (self.clock)()
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

    /// 不改变任何状态，先判断这个键会不会被输入法拿走（Windows 的 OnTestKeyDown 要用）。
    /// 必须和 `process_key` 的结果一致。
    pub fn would_consume(&self, s: &Session, key: Key, mods: Mods) -> bool {
        let key = if key == Key::Char(' ') { Key::Space } else { key };
        if key == Key::ShiftTap {
            return true;
        }
        if self.english_mode || mods.ctrl || mods.alt {
            return false;
        }
        if s.is_composing() {
            return true;
        }
        match key {
            Key::Char(c) if c.is_ascii_alphabetic() => !mods.caps_lock,
            Key::Char(c) if self.config.chinese_punct => ZH_PUNCT_KEYS.contains(c),
            _ => false,
        }
    }

    /// 处理一个按键。
    pub fn process_key(&mut self, s: &mut Session, key: Key, mods: Mods) -> Response {
        let key = if key == Key::Char(' ') { Key::Space } else { key };
        // 单按 Shift：中文 ↔ 纯英文。正在打拼音时，先把打的字母原样上屏
        if key == Key::ShiftTap {
            let commit = if s.is_composing() { self.commit_raw(s).commit } else { None };
            self.english_mode = !self.english_mode;
            s.clear_context();
            return Response { consumed: true, commit };
        }
        // 纯英文：所有键原样交给程序
        if self.english_mode {
            return Response::pass();
        }
        // Ctrl/Alt 组合键（复制、保存、切窗口……）一律交给程序。
        // 正在打拼音时先把打的字母原样上屏，免得快捷键作用在半截的输入上
        if mods.ctrl || mods.alt {
            let commit = if s.is_composing() { self.commit_raw(s).commit } else { None };
            s.clear_context();
            return Response { consumed: false, commit };
        }
        if !s.is_composing() {
            if let Key::Char(c) = key {
                if c.is_ascii_alphabetic() && !mods.caps_lock {
                    self.push_char(s, c);
                    return Response::eaten();
                }
                // 中文标点开着：符号换成中文的
                if self.config.chinese_punct {
                    if let Some(p) = zh_punct(c, s) {
                        s.clear_context();
                        return Response { consumed: true, commit: Some(p.to_string()) };
                    }
                }
            }
            // 原样交给程序的键（符号、空格、回车、方向键……）：上文断开。
            // 数字例外：它是"前面是数字"这个上文（2010 后面打 n，"年"排第一）
            let digit = matches!(key, Key::Char(c) if c.is_ascii_digit());
            s.ctx =
                if digit { self.lm.as_ref().and_then(|l| l.id(NUM_TOKEN)).map(|id| (id, self.now())) } else { None };
            return Response::pass();
        }
        match s.mode {
            Mode::Row => self.key_row(s, key),
            Mode::Grid => self.key_grid(s, key),
        }
    }

    fn key_row(&mut self, s: &mut Session, key: Key) -> Response {
        match key {
            Key::Char(c) if c.is_ascii_alphabetic() => {
                self.push_char(s, c);
                Response::eaten()
            }
            Key::Char('\'') => {
                if !s.input.ends_with('\'') {
                    self.push_char(s, '\'');
                }
                Response::eaten()
            }
            Key::Char(d @ '1'..='9') => {
                let i = d as usize - '1' as usize;
                if i < s.row_len {
                    return self.select(s, i);
                }
                Response::eaten()
            }
            Key::Char('0') => Response::eaten(),
            Key::Char('-') => {
                if !s.cands.is_empty() {
                    s.mode = Mode::Grid;
                    // 高亮停在候选条里没显示过的第一个
                    s.hl = s.row_len.min(s.cands.len() - 1);
                }
                Response::eaten()
            }
            Key::Char(c) => self.commit_with(s, Some(c)),
            Key::Space => {
                if s.cands.is_empty() {
                    self.commit_with(s, None)
                } else {
                    let hl = s.hl;
                    self.select(s, hl)
                }
            }
            Key::Enter => self.commit_raw(s),
            Key::Escape => {
                s.reset();
                Response::eaten()
            }
            Key::Backspace => {
                self.backspace(s);
                Response::eaten()
            }
            Key::Left => {
                s.hl = s.hl.saturating_sub(1);
                Response::eaten()
            }
            Key::Right => {
                if s.hl + 1 < s.row_len {
                    s.hl += 1;
                }
                Response::eaten()
            }
            Key::Up | Key::Down | Key::Other | Key::ShiftTap => Response::eaten(),
        }
    }

    fn key_grid(&mut self, s: &mut Session, key: Key) -> Response {
        let len = s.cands.len();
        match key {
            Key::Char(c) if c.is_ascii_alphabetic() || c == '\'' => {
                s.mode = Mode::Row;
                self.key_row(s, key)
            }
            Key::Char(d @ '1'..='5') => {
                let row_start = s.hl / GRID_COLS * GRID_COLS;
                let i = row_start + (d as usize - '1' as usize);
                if i < len {
                    return self.select(s, i);
                }
                Response::eaten()
            }
            Key::Char(c) if c.is_ascii_digit() => Response::eaten(),
            Key::Char('-') | Key::Escape => {
                s.mode = Mode::Row;
                s.hl = 0;
                Response::eaten()
            }
            Key::Char(c) => self.commit_with(s, Some(c)),
            // 矩阵里回车和空格一样：上屏高亮的候选
            Key::Space | Key::Enter => {
                let hl = s.hl;
                self.select(s, hl)
            }
            Key::Backspace => {
                self.backspace(s);
                Response::eaten()
            }
            Key::Left => {
                s.hl = s.hl.saturating_sub(1);
                Response::eaten()
            }
            Key::Right => {
                if s.hl + 1 < len {
                    s.hl += 1;
                }
                Response::eaten()
            }
            Key::Up => {
                if s.hl >= GRID_COLS {
                    s.hl -= GRID_COLS;
                }
                Response::eaten()
            }
            Key::Down => {
                if s.hl + GRID_COLS < len {
                    s.hl += GRID_COLS;
                } else if s.hl / GRID_COLS < (len - 1) / GRID_COLS {
                    // 下一行不满，跳到最后一个
                    s.hl = len - 1;
                }
                Response::eaten()
            }
            Key::Other | Key::ShiftTap => Response::eaten(),
        }
    }

    fn push_char(&mut self, s: &mut Session, c: char) {
        if s.input.len() < self.config.max_input {
            s.input.push(c);
            self.refresh(s);
        }
    }

    fn refresh(&mut self, s: &mut Session) {
        let now = self.now();
        // 上文：这次已经选了字就用最后选的那个词，否则用刚上屏的词（30 秒内）
        let prev = match s.selected.last() {
            Some(x) => x.last_lm,
            None => s.ctx.filter(|&(_, t)| now - t <= CONTEXT_SECS).map(|(id, _)| id),
        };
        let q = Query { dict: &self.dict, en: self.en.as_ref(), lm: self.lm.as_ref(), user: &self.user, prev, now };
        let cands = {
            let rem = s.remaining();
            if rem.is_empty() {
                Vec::new()
            } else {
                analyze_with(&q, rem)
            }
        };
        s.cands = cands;
        s.row_len = self.compute_row_len(&s.cands);
        s.hl = 0;
        s.mode = Mode::Row;
    }

    fn compute_row_len(&self, cands: &[Candidate]) -> usize {
        let cfg = &self.config;
        let (mut width, mut k) = (0usize, 0usize);
        for c in cands {
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

    fn select(&mut self, s: &mut Session, i: usize) -> Response {
        let Some(c) = s.cands.get(i).cloned() else { return Response::eaten() };
        self.note_rejection(s, &c);
        self.take(s, c);
        if s.consumed >= s.input.len() {
            self.finish(s, None)
        } else {
            self.refresh(s);
            Response::eaten()
        }
    }

    /// 你选的多字词不是"本来该排第一"的那个：给那个词记一次冷落（单字不算）。
    fn note_rejection(&mut self, s: &Session, chosen: &Candidate) {
        if !matches!(chosen.kind, CandKind::Word | CandKind::Sentence) || chosen.text.chars().count() < 2 {
            return;
        }
        let top = s
            .cands
            .iter()
            .filter(|c| matches!(c.kind, CandKind::Word | CandKind::Sentence) && c.consumed == chosen.consumed)
            .max_by(|a, b| a.base.total_cmp(&b.base));
        if let Some(t) = top {
            if t.kind == CandKind::Word
                && t.text != chosen.text
                && t.text.chars().count() >= 2
                && t.fixes.is_empty()
                && !t.code.is_empty()
            {
                self.user.reject(&self.dict, &t.code, &t.text);
            }
        }
    }

    /// 把一个候选放进"已选"，往后移动输入位置。
    fn take(&mut self, s: &mut Session, c: Candidate) {
        let start = s.consumed;
        match c.kind {
            CandKind::Raw | CandKind::English => {
                let mut x = Selected::plain(start, c.text);
                x.english = c.kind == CandKind::English || x.text.len() >= 3;
                s.selected.push(x);
                s.consumed = s.input.len();
            }
            _ => {
                s.selected.push(Selected {
                    start,
                    text: c.text,
                    code: c.code,
                    learnable: c.learnable,
                    fixes: c.fixes,
                    typed_code: c.typed_code,
                    english: false,
                    last_lm: c.last_lm,
                });
                s.consumed = skip_apos(s.input.as_bytes(), start + c.consumed).min(s.input.len());
            }
        }
    }

    /// 上屏全部已选的字，记住用过的词，然后清空。`extra` 是跟在后面的符号。
    fn finish(&mut self, s: &mut Session, extra: Option<String>) -> Response {
        let now = self.now();
        let mut text: String = s.selected.iter().map(|x| x.text.as_str()).collect();
        // 前后词关联：记住"上一个词后面选了什么"（上一次上屏的词，以及这次分几段选的前后两段）
        if let Some(lm) = &self.lm {
            let mut prev = s.ctx.filter(|&(_, t)| now - t <= CONTEXT_SECS).map(|(id, _)| id);
            for x in &s.selected {
                if let Some(p) = prev {
                    let pt = lm.word(p);
                    if !pt.starts_with('<') && !x.english && !x.code.is_empty() {
                        self.user.learn_pair(pt, &x.text, now);
                    }
                }
                prev = x.last_lm;
            }
        }
        for x in &s.selected {
            if x.learnable && !x.code.is_empty() {
                self.user.learn(&self.dict, &x.code, &x.text, now);
            }
            // 单字：慢慢记（选几十次才会换位置）
            if x.code.len() == 1 && x.text.chars().count() == 1 {
                self.user.learn_char(&self.dict, x.code[0], &x.text, now);
            }
            // 打错被纠正了：记住这种错法，下次同样的错扣分更少
            for f in &x.fixes {
                let (a, b) = f.key();
                self.user.learn_fix(&a, &b, now);
            }
            // 再记住"这样打错就是想要这个词"，下次一模一样的错直接排第一
            if let Some(tc) = &x.typed_code {
                self.user.learn(&self.dict, tc, &x.text, now);
            }
            if x.english {
                self.user.learn_en(&x.text, now);
            }
        }
        // 分几次选出来的，合起来存成一个新词（比如人名）
        if s.selected.len() >= 2 && s.selected.iter().all(|x| x.learnable && !x.code.is_empty()) {
            let n = text.chars().count();
            if (2..=8).contains(&n) {
                let code: Vec<u16> = s.selected.iter().flat_map(|x| x.code.iter().copied()).collect();
                self.user.learn(&self.dict, &code, &text, now);
            }
        }
        // 记下上文给下一次输入用；后面跟了符号就算断句
        s.ctx = match (&extra, s.selected.last().and_then(|x| x.last_lm)) {
            (None, Some(id)) => Some((id, now)),
            _ => None,
        };
        if let Some(e) = extra {
            text.push_str(&e);
        }
        s.reset();
        Response { consumed: true, commit: Some(text) }
    }

    /// 按了符号（或者没有候选时按空格）：先上屏高亮的候选，再输出符号。
    fn commit_with(&mut self, s: &mut Session, sym: Option<char>) -> Response {
        let rem = s.remaining().len();
        if rem > 0 {
            let covers = |c: &Candidate| c.consumed >= rem;
            let pick = match s.cands.get(s.hl) {
                Some(c) if covers(c) => Some(c.clone()),
                _ => s.cands.first().filter(|c| covers(c)).cloned(),
            };
            match pick {
                Some(c) => self.take(s, c),
                None => {
                    let raw = s.remaining().to_string();
                    let start = s.consumed;
                    s.selected.push(Selected::plain(start, raw));
                    s.consumed = s.input.len();
                }
            }
        }
        let extra = sym.map(|c| {
            if self.config.chinese_punct {
                if let Some(p) = zh_punct(c, s) {
                    return p.to_string();
                }
            }
            c.to_string()
        });
        self.finish(s, extra)
    }

    /// 回车：已选的字 + 剩下的字母原样上屏。
    fn commit_raw(&mut self, s: &mut Session) -> Response {
        let mut text: String = s.selected.iter().map(|x| x.text.as_str()).collect();
        text.push_str(s.remaining());
        s.clear_context();
        s.reset();
        Response { consumed: true, commit: Some(text) }
    }

    fn backspace(&mut self, s: &mut Session) {
        if s.input.len() > s.consumed {
            s.input.pop();
        } else if let Some(x) = s.selected.pop() {
            s.consumed = x.start;
        }
        if s.input.is_empty() {
            s.reset();
        } else {
            self.refresh(s);
        }
    }

    pub fn view(&self, s: &Session) -> View {
        let mut preedit: String = s.selected.iter().map(|x| x.text.as_str()).collect();
        preedit.push_str(s.remaining());
        let total = s.cands.len();
        let (range, highlight, page, pages) = match s.mode {
            Mode::Row => (0..s.row_len, s.hl.min(s.row_len.saturating_sub(1)), 0, 1),
            Mode::Grid => {
                let page = s.hl / GRID_SIZE;
                let start = page * GRID_SIZE;
                let end = (start + GRID_SIZE).min(total);
                (start..end, s.hl - start, page, total.div_ceil(GRID_SIZE))
            }
        };
        let shown = &s.cands[range];
        View {
            composing: s.is_composing(),
            preedit,
            mode: s.mode,
            items: shown.iter().map(|c| c.text.clone()).collect(),
            highlight,
            page,
            pages,
            total,
            corrected: shown.iter().map(|c| c.corrected()).collect(),
            english_mode: self.english_mode,
        }
    }
}

/// 一个 Engine 配一个 Session：单个程序用（试打工具、测试）。
pub struct Ime {
    engine: Engine,
    session: Session,
}

impl Ime {
    pub fn new(dict: Dict, user: UserDb) -> Self {
        Ime { engine: Engine::new(dict, user), session: Session::new() }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    pub fn set_english(&mut self, en: EnglishDict) {
        self.engine.set_english(en);
    }

    pub fn set_lm(&mut self, lm: Lm) {
        self.engine.set_lm(lm);
    }

    pub fn lm(&self) -> Option<&Lm> {
        self.engine.lm()
    }

    pub fn english_mode(&self) -> bool {
        self.engine.english_mode()
    }

    pub fn set_english_mode(&mut self, on: bool) {
        if on && self.session.is_composing() {
            self.session.reset();
        }
        self.engine.set_english_mode(on);
        self.session.clear_context();
    }

    pub fn clear_context(&mut self) {
        self.session.clear_context();
    }

    pub fn set_clock(&mut self, f: impl Fn() -> i64 + Send + 'static) {
        self.engine.set_clock(f);
    }

    pub fn dict(&self) -> &Dict {
        self.engine.dict()
    }

    pub fn user(&self) -> &UserDb {
        self.engine.user()
    }

    pub fn user_mut(&mut self) -> &mut UserDb {
        self.engine.user_mut()
    }

    pub fn is_composing(&self) -> bool {
        self.session.is_composing()
    }

    pub fn candidates(&self) -> &[Candidate] {
        self.session.candidates()
    }

    pub fn process_key(&mut self, key: Key, mods: Mods) -> Response {
        self.engine.process_key(&mut self.session, key, mods)
    }

    pub fn would_consume(&self, key: Key, mods: Mods) -> bool {
        self.engine.would_consume(&self.session, key, mods)
    }

    pub fn reset(&mut self) {
        self.session.reset();
    }

    pub fn view(&self) -> View {
        self.engine.view(&self.session)
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
            add("xian", &format!("先{i}"), -6.0 - i as f32 * 0.3);
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
        assert!(!m.view().items.is_empty());
        key(&mut m, Key::Up);
        assert_eq!(m.view().page, 0);
        // 数字选高亮那一行的第几个
        let v = m.view();
        let row = v.highlight / 5 * 5;
        let want = v.items[row + 2].clone();
        assert_eq!(key(&mut m, Key::Char('3')).commit, Some(want));
    }

    #[test]
    fn chinese_punctuation_switch() {
        let mut m = ime();
        m.engine_mut().config.chinese_punct = true;
        assert_eq!(key(&mut m, Key::Char(',')).commit.as_deref(), Some("，"));
        assert_eq!(typ(&mut m, "nihao,"), "你好，");
        assert_eq!(typ(&mut m, "nihao?"), "你好？");
        // 引号一左一右
        assert_eq!(key(&mut m, Key::Char('"')).commit.as_deref(), Some("“"));
        assert_eq!(key(&mut m, Key::Char('"')).commit.as_deref(), Some("”"));
        // 开着时全部是中文标点，没有例外（数字后面也是）
        assert_eq!(key(&mut m, Key::Char('3')), Response::pass());
        assert_eq!(key(&mut m, Key::Char('.')).commit.as_deref(), Some("。"));
        for (k, want) in [
            ('(', "（"),
            ('[', "【"),
            ('{', "｛"),
            ('<', "《"),
            ('\\', "、"),
            ('^', "……"),
            ('_', "——"),
            ('$', "￥"),
            ('`', "·"),
        ] {
            assert_eq!(key(&mut m, Key::Char(k)).commit.as_deref(), Some(want), "{k}");
        }
        // 空格永远是半角，原样交给程序
        assert_eq!(key(&mut m, Key::Space), Response::pass());
        // 关掉以后全是英文
        m.engine_mut().config.chinese_punct = false;
        for c in ",.?!:;()[]{}<>\\^_$~`\"'".chars() {
            assert_eq!(key(&mut m, Key::Char(c)), Response::pass(), "{c}");
        }
        assert_eq!(typ(&mut m, "nihao,"), "你好,");
    }

    #[test]
    fn would_consume_matches_process_key() {
        // Windows 先问"这个键要不要"，再真的处理，两边的答案必须一样
        let mut m = ime();
        let keys = [
            Key::Char('n'),
            Key::Char('i'),
            Key::Char(','),
            Key::Char('3'),
            Key::Char('.'),
            Key::Char('-'),
            Key::Char('2'),
            Key::Char('\''),
            Key::Space,
            Key::Enter,
            Key::Backspace,
            Key::Escape,
            Key::Left,
            Key::Down,
            Key::ShiftTap,
            Key::Char('X'),
            Key::Other,
        ];
        let mut x: u64 = 12345;
        for step in 0..5000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let k = keys[(x % keys.len() as u64) as usize];
            let mods = Mods { ctrl: step % 97 == 0, alt: false, caps_lock: step % 211 == 0 };
            if step % 500 == 0 {
                let p = m.engine().config.chinese_punct;
                m.engine_mut().config.chinese_punct = !p;
            }
            let predicted = m.would_consume(k, mods);
            let actual = m.process_key(k, mods).consumed;
            assert_eq!(predicted, actual, "第 {step} 步，按键 {k:?}，{mods:?}");
        }
    }

    #[test]
    fn enter_in_grid_selects_like_space() {
        let mut m = ime();
        typ(&mut m, "xian");
        key(&mut m, Key::Char('-'));
        key(&mut m, Key::Down);
        let v = m.view();
        let want = v.items[v.highlight].clone();
        assert_eq!(key(&mut m, Key::Enter).commit, Some(want));
        // 候选条里回车还是原样上屏字母
        typ(&mut m, "xian");
        assert_eq!(key(&mut m, Key::Enter).commit.as_deref(), Some("xian"));
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
    fn single_chars_reorder_only_after_many_picks() {
        let mut m = ime();
        // 选 5 次"事"："是"还在第一
        for _ in 0..5 {
            assert_eq!(typ(&mut m, "shi2"), "事");
        }
        typ(&mut m, "shi");
        assert_eq!(m.view().items[0], "是");
        key(&mut m, Key::Escape);
        // 一直选"事"，选到几十次以后"事"排第一
        let mut n = 5;
        loop {
            typ(&mut m, "shi");
            if m.view().items[0] == "事" {
                break;
            }
            let i = m.view().items.iter().position(|t| t == "事").unwrap();
            key(&mut m, Key::Char(char::from(b'1' + i as u8)));
            n += 1;
            assert!(n < 200, "选了 200 次还没换");
        }
        assert!(n >= 20, "只选了 {n} 次就换了，太快");
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
        // 打到一半按快捷键：字母原样上屏，快捷键照样交给程序
        typ(&mut m, "nihao");
        assert!(!m.would_consume(Key::Char('s'), ctrl));
        assert_eq!(m.process_key(Key::Char('s'), ctrl), Response { consumed: false, commit: Some("nihao".into()) });
        assert!(!m.is_composing());
    }

    #[test]
    fn escape_clears() {
        let mut m = ime();
        typ(&mut m, "nihao");
        key(&mut m, Key::Escape);
        assert!(!m.is_composing());
    }

    #[test]
    fn previous_word_is_used_as_context() {
        use crate::lm::{Lm, LmBuilder};
        let mut b = DictBuilder::new(["zou", "ba"].map(String::from));
        let zou = b.code("zou").unwrap();
        let ba = b.code("ba").unwrap();
        b.add(&zou, "走", -6.0);
        b.add(&ba, "把", -4.0);
        b.add(&ba, "吧", -6.0);
        let d = Dict::from_bytes(b.build()).unwrap();
        // 词表按字节排序：吧 把 走；"走"后面常跟"吧"
        let lm = LmBuilder {
            vocab: ["吧", "把", "走"].map(String::from).to_vec(),
            uni: vec![-6.0, -4.0, -6.0],
            backoff: vec![-1.0, -1.0, -1.0],
            succ: vec![vec![], vec![], vec![(0, -0.3)]],
            fallback_offset: 0.0,
        };
        let mut m = Ime::new(d, UserDb::in_memory());
        m.set_lm(Lm::from_bytes(lm.build()).unwrap());
        m.set_clock(|| 1000);
        // 没有上文：把
        typ(&mut m, "ba");
        assert_eq!(m.view().items[0], "把");
        key(&mut m, Key::Escape);
        // 先打"走"上屏，再打 ba：吧
        assert_eq!(typ(&mut m, "zou "), "走");
        typ(&mut m, "ba");
        assert_eq!(m.view().items[0], "吧");
        key(&mut m, Key::Escape);
        // 中间打了个逗号（原样交给程序），上文断开
        assert_eq!(typ(&mut m, "zou "), "走");
        assert_eq!(key(&mut m, Key::Char(',')), Response::pass());
        typ(&mut m, "ba");
        assert_eq!(m.view().items[0], "把");
    }

    #[test]
    fn last_pick_far_back_comes_to_front_four() {
        let mut m = ime();
        // xian 有 40 多个候选；从矩阵里选第 20 个
        typ(&mut m, "xian");
        let first = m.view().items[0].clone();
        let want = m.candidates()[20].text.clone();
        key(&mut m, Key::Char('-'));
        while m.view().page * 25 + m.view().highlight < 20 {
            key(&mut m, Key::Right);
        }
        assert_eq!(key(&mut m, Key::Space).commit, Some(want.clone()));
        // 下次：第一个不变，选过的那个在前 4 个里
        typ(&mut m, "xian");
        let row = m.view().items;
        assert_eq!(row[0], first);
        assert!(row.iter().take(4).any(|t| *t == want), "{want} 不在前 4 个：{row:?}");
    }

    #[test]
    fn number_before_changes_choice() {
        use crate::lm::{Lm, LmBuilder};
        let mut b = DictBuilder::new(["ni", "nian"].map(String::from));
        let ni = b.code("ni").unwrap();
        let nian = b.code("nian").unwrap();
        b.add(&ni, "你", -5.0);
        b.add(&nian, "年", -6.0);
        let d = Dict::from_bytes(b.build()).unwrap();
        // 词表按字节排序：<数字> 你 年；数字后面常跟"年"
        let lm = LmBuilder {
            vocab: [NUM_TOKEN, "你", "年"].map(String::from).to_vec(),
            uni: vec![-8.0, -4.0, -7.0],
            backoff: vec![-2.0, -1.0, -1.0],
            succ: vec![vec![(2, -0.5)], vec![], vec![]],
            fallback_offset: 0.0,
        };
        let mut m = Ime::new(d, UserDb::in_memory());
        m.set_lm(Lm::from_bytes(lm.build()).unwrap());
        m.set_clock(|| 1000);
        typ(&mut m, "n");
        assert_eq!(m.view().items[0], "你");
        key(&mut m, Key::Escape);
        assert_eq!(typ(&mut m, "2010"), "2010");
        typ(&mut m, "n");
        assert_eq!(m.view().items[0], "年");
    }

    #[test]
    fn shift_tap_switches_to_pure_english() {
        let mut m = ime();
        // 切到英文：所有键原样交给程序
        assert_eq!(key(&mut m, Key::ShiftTap), Response { consumed: true, commit: None });
        assert!(m.view().english_mode);
        for k in [Key::Char('n'), Key::Char('i'), Key::Space, Key::Char(','), Key::Enter, Key::Char('-')] {
            assert_eq!(key(&mut m, k), Response::pass());
        }
        assert!(!m.is_composing());
        // 再按一下回到中文
        key(&mut m, Key::ShiftTap);
        assert!(!m.view().english_mode);
        assert_eq!(typ(&mut m, "nihao "), "你好");
        // 打拼音打到一半按 Shift：字母原样上屏，然后是英文
        typ(&mut m, "nihao");
        assert_eq!(key(&mut m, Key::ShiftTap).commit.as_deref(), Some("nihao"));
        assert!(m.view().english_mode);
        assert!(!m.is_composing());
        // Shift+字母（大写）不会切换
        key(&mut m, Key::ShiftTap);
        typ(&mut m, "Ni");
        assert!(m.is_composing());
        assert!(!m.view().english_mode);
    }

    #[test]
    fn output_is_never_full_width() {
        // 空格、标点、字母、数字永远是英文半角，不会出现全角空格（U+3000）或全角符号（U+FF01–FF5E）
        let mut m = ime();
        let full = |s: &str| s.chars().any(|c| c == '\u{3000}' || ('\u{FF01}'..='\u{FF5E}').contains(&c));
        let mut out = String::new();
        for s in ["nihao ", "nihao,", "shi.", "a!", "zhang zi han?", "ni hao;", "xian'"] {
            out.push_str(&typ(&mut m, s));
            key(&mut m, Key::Escape);
        }
        assert!(!full(&out), "{out:?}");
        for c in " ,.!?;:'\"()[]{}<>@#$%^&*-_=+/\\|`~1234567890".chars() {
            assert_eq!(key(&mut m, Key::Char(c)), Response::pass(), "{c:?}");
        }
    }

    #[test]
    fn apostrophe_only_inside_composition() {
        let mut m = ime();
        assert_eq!(key(&mut m, Key::Char('\'')), Response::pass());
        typ(&mut m, "xi'an");
        assert_eq!(m.view().items[0], "西安");
    }
}
