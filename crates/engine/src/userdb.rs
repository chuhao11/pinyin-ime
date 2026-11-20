//! 用户词库：记住你选过的词和自己拼出来的词。
//!
//! - 词和人名选一次就排第一；单字记得很慢（要选几十次才会换位置，"是"不会因为偶尔选了"事"就掉下去）。
//! - 每个词记"有效次数"和最后使用时间；有效次数随时间按半衰期衰减，所以很久不用的词会慢慢退回去。
//! - 条数有上限，超过后淘汰有效次数最低的，内存和文件都不会无限增长。
//! - 存在本机 SQLite 文件里（WAL 模式，断电也不会损坏）；不保存任何打字记录。
//! - 另外记三样小东西：你接受过哪些打错纠正（u→i 这种）、用过的英文词、
//!   以及"首选是它但你总选别的"的词（被冷落 3 次就删掉）。

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection};

use crate::dict::Dict;

/// 半衰期：90 天不用，分量减半。
pub const HALF_LIFE_SECS: f64 = 90.0 * 86400.0;
/// 默认最多记 10 万条。
pub const DEFAULT_MAX_ENTRIES: usize = 100_000;
/// 英文词、冷落记录各最多记这么多条。
pub const MAX_SMALL_ENTRIES: usize = 5_000;
/// 首选被冷落这么多次就删掉。
pub const REJECT_LIMIT: u32 = 3;
/// 单字最多记这么多条。
pub const MAX_CHAR_ENTRIES: usize = 20_000;

/// 单字的记忆加分：很慢地增长，选 5 次约 +0.6，选 40 次约 +2.2，选 300 次约 +4.4。
pub fn char_bonus(eff: f64) -> f32 {
    (1.2 * (1.0 + eff / 8.0).ln()) as f32
}

/// 记住的英文词。
pub struct UEn {
    pub text: String,
    pub freq: f64,
    pub last: i64,
}

pub struct UEntry {
    pub code: Vec<u16>,
    pub text: String,
    pub freq: f64,
    pub last: i64,
}

impl UEntry {
    pub fn effective(&self, now: i64) -> f64 {
        effective(self.freq, self.last, now)
    }
}

pub fn effective(freq: f64, last: i64, now: i64) -> f64 {
    let dt = (now - last).max(0) as f64;
    freq * 0.5f64.powf(dt / HALF_LIFE_SECS)
}

/// 记忆加分：用过一次且是最近，加分足够让它排第一；随时间衰减。
pub fn bonus(eff: f64) -> f32 {
    (25.0 * eff.min(1.0) + 3.0 * (1.0 + eff).ln()) as f32
}

#[derive(Default)]
struct UNode {
    children: Vec<(u16, u32)>,
    entries: Vec<u32>,
}

pub struct UserDb {
    conn: Option<Connection>,
    entries: Vec<UEntry>,
    by_text: HashMap<String, Vec<u32>>,
    nodes: Vec<UNode>,
    blocked: HashMap<String, Vec<Vec<u16>>>,
    max_entries: usize,
    write_errors: u64,
    /// 接受过的纠正：(打的, 改成) → (有效次数, 最后时间)
    fixes: HashMap<(String, String), (f64, i64)>,
    /// 英文词：小写 → 记录
    en: HashMap<String, UEn>,
    /// 首选被冷落的次数
    rejects: HashMap<(Vec<u16>, String), u32>,
    /// 单字：(音节, 字) → (有效次数, 最后时间)
    chars: HashMap<(u16, String), (f64, i64)>,
}

fn code_to_string(dict: &Dict, code: &[u16]) -> String {
    code.iter().map(|&c| dict.syllable(c)).collect::<Vec<_>>().join(" ")
}

fn code_from_string(dict: &Dict, s: &str) -> Option<Vec<u16>> {
    let v: Option<Vec<u16>> = s.split_whitespace().map(|p| dict.syllable_id(p)).collect();
    v.filter(|v| !v.is_empty())
}

impl UserDb {
    /// 不落盘的用户词库（测试用）。
    pub fn in_memory() -> Self {
        UserDb {
            conn: None,
            entries: Vec::new(),
            by_text: HashMap::new(),
            nodes: vec![UNode::default()],
            blocked: HashMap::new(),
            max_entries: DEFAULT_MAX_ENTRIES,
            write_errors: 0,
            fixes: HashMap::new(),
            en: HashMap::new(),
            rejects: HashMap::new(),
            chars: HashMap::new(),
        }
    }

    /// 打开（没有就新建）用户词库文件。
    pub fn open(path: &Path, dict: &Dict) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("打不开用户词库 {}：{e}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-2000;
             PRAGMA journal_size_limit=4194304;
             CREATE TABLE IF NOT EXISTS user_words(
                 code TEXT NOT NULL, word TEXT NOT NULL,
                 freq REAL NOT NULL, last INTEGER NOT NULL,
                 PRIMARY KEY(code, word)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS blocked_words(
                 code TEXT NOT NULL, word TEXT NOT NULL,
                 PRIMARY KEY(code, word)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS typo_fixes(
                 typed TEXT NOT NULL, meant TEXT NOT NULL,
                 freq REAL NOT NULL, last INTEGER NOT NULL,
                 PRIMARY KEY(typed, meant)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS english_words(
                 key TEXT NOT NULL PRIMARY KEY, word TEXT NOT NULL,
                 freq REAL NOT NULL, last INTEGER NOT NULL) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS rejected(
                 code TEXT NOT NULL, word TEXT NOT NULL, count INTEGER NOT NULL,
                 PRIMARY KEY(code, word)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS single_chars(
                 code TEXT NOT NULL, word TEXT NOT NULL,
                 freq REAL NOT NULL, last INTEGER NOT NULL,
                 PRIMARY KEY(code, word)) WITHOUT ROWID;",
        )
        .map_err(|e| format!("初始化用户词库失败：{e}"))?;

        let mut db = UserDb::in_memory();
        {
            let mut st = conn.prepare("SELECT code, word, freq, last FROM user_words").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?))
                })
                .map_err(|e| e.to_string())?;
            for row in rows.flatten() {
                let (code, text, freq, last) = row;
                // 词库升级后不认识的拼音直接跳过
                if let Some(code) = code_from_string(dict, &code) {
                    if text.chars().count() >= 2 && freq.is_finite() && freq > 0.0 {
                        db.entries.push(UEntry { code, text, freq, last });
                    }
                }
            }
            let mut st = conn.prepare("SELECT code, word FROM blocked_words").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            for (code, text) in rows.flatten() {
                if let Some(code) = code_from_string(dict, &code) {
                    db.blocked.entry(text).or_default().push(code);
                }
            }
            let mut st = conn.prepare("SELECT typed, meant, freq, last FROM typo_fixes").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?))
                })
                .map_err(|e| e.to_string())?;
            for (a, b, f, l) in rows.flatten() {
                if f.is_finite() && f > 0.0 {
                    db.fixes.insert((a, b), (f, l));
                }
            }
            let mut st = conn.prepare("SELECT key, word, freq, last FROM english_words").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?))
                })
                .map_err(|e| e.to_string())?;
            for (k, text, freq, last) in rows.flatten() {
                if freq.is_finite() && freq > 0.0 {
                    db.en.insert(k, UEn { text, freq, last });
                }
            }
            let mut st = conn.prepare("SELECT code, word, count FROM rejected").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?)))
                .map_err(|e| e.to_string())?;
            for (code, text, n) in rows.flatten() {
                if let Some(code) = code_from_string(dict, &code) {
                    db.rejects.insert((code, text), n);
                }
            }
            let mut st = conn.prepare("SELECT code, word, freq, last FROM single_chars").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?))
                })
                .map_err(|e| e.to_string())?;
            for (code, text, f, l) in rows.flatten() {
                if let (Some(id), true) = (dict.syllable_id(&code), f.is_finite() && f > 0.0) {
                    db.chars.insert((id, text), (f, l));
                }
            }
        }
        db.conn = Some(conn);
        db.rebuild();
        Ok(db)
    }

    pub fn set_max_entries(&mut self, n: usize) {
        self.max_entries = n.max(10);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 写盘失败的次数（不让它影响打字，只记下来）。
    pub fn write_errors(&self) -> u64 {
        self.write_errors
    }

    fn rebuild(&mut self) {
        self.by_text.clear();
        self.nodes = vec![UNode::default()];
        for i in 0..self.entries.len() {
            self.index(i as u32);
        }
    }

    fn index(&mut self, idx: u32) {
        let e = &self.entries[idx as usize];
        self.by_text.entry(e.text.clone()).or_default().push(idx);
        let mut n = 0usize;
        for &s in &e.code {
            n = match self.nodes[n].children.binary_search_by_key(&s, |c| c.0) {
                Ok(i) => self.nodes[n].children[i].1 as usize,
                Err(i) => {
                    let id = self.nodes.len() as u32;
                    self.nodes.push(UNode::default());
                    self.nodes[n].children.insert(i, (s, id));
                    id as usize
                }
            };
        }
        self.nodes[n].entries.push(idx);
    }

    fn find(&self, code: &[u16], text: &str) -> Option<u32> {
        self.by_text.get(text)?.iter().copied().find(|&i| self.entries[i as usize].code == code)
    }

    pub fn lookup(&self, code: &[u16], text: &str) -> Option<&UEntry> {
        self.find(code, text).map(|i| &self.entries[i as usize])
    }

    /// 这个词的记忆加分（没记过、或者是单字，返回 0）。
    pub fn bonus(&self, code: &[u16], text: &str, now: i64) -> f32 {
        self.lookup(code, text).map(|e| bonus(e.effective(now))).unwrap_or(0.0)
    }

    pub fn is_blocked(&self, code: &[u16], text: &str) -> bool {
        self.blocked.get(text).is_some_and(|v| v.iter().any(|c| c == code))
    }

    /// 记住一次使用。单字不记。
    pub fn learn(&mut self, dict: &Dict, code: &[u16], text: &str, now: i64) {
        if code.is_empty() || text.chars().count() < 2 || self.is_blocked(code, text) {
            return;
        }
        // 你选了它，之前的冷落记录作废
        if self.rejects.remove(&(code.to_vec(), text.to_string())).is_some() {
            if let Some(conn) = &self.conn {
                let _ = conn.execute(
                    "DELETE FROM rejected WHERE code = ?1 AND word = ?2",
                    params![code_to_string(dict, code), text],
                );
            }
        }
        let (freq, last) = match self.find(code, text) {
            Some(i) => {
                let e = &mut self.entries[i as usize];
                e.freq = e.effective(now) + 1.0;
                e.last = now;
                (e.freq, e.last)
            }
            None => {
                self.entries.push(UEntry { code: code.to_vec(), text: text.to_string(), freq: 1.0, last: now });
                self.index((self.entries.len() - 1) as u32);
                (1.0, now)
            }
        };
        if let Some(conn) = &self.conn {
            let r = conn.execute(
                "INSERT INTO user_words(code, word, freq, last) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(code, word) DO UPDATE SET freq = excluded.freq, last = excluded.last",
                params![code_to_string(dict, code), text, freq, last],
            );
            if r.is_err() {
                self.write_errors += 1;
            }
        }
        if self.entries.len() > self.max_entries {
            self.evict(dict, now);
        }
    }

    /// 删掉一个词，以后也不再出现在候选里（词库自带的词也可以删）。
    pub fn forget(&mut self, dict: &Dict, code: &[u16], text: &str) {
        if let Some(i) = self.find(code, text) {
            self.entries.swap_remove(i as usize);
            self.rebuild();
        }
        if !self.is_blocked(code, text) {
            self.blocked.entry(text.to_string()).or_default().push(code.to_vec());
        }
        if let Some(conn) = &self.conn {
            let c = code_to_string(dict, code);
            let a = conn.execute("DELETE FROM user_words WHERE code = ?1 AND word = ?2", params![c, text]);
            let b = conn.execute("INSERT OR IGNORE INTO blocked_words(code, word) VALUES (?1, ?2)", params![c, text]);
            if a.is_err() || b.is_err() {
                self.write_errors += 1;
            }
        }
    }

    /// 超过上限时，淘汰有效次数最低的词，留下上限的 90%。
    fn evict(&mut self, dict: &Dict, now: i64) {
        let keep = self.max_entries * 9 / 10;
        if self.entries.len() <= keep {
            return;
        }
        let mut order: Vec<(f64, usize)> =
            self.entries.iter().enumerate().map(|(i, e)| (e.effective(now), i)).collect();
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        let remove_n = self.entries.len() - keep;
        let mut drop = vec![false; self.entries.len()];
        for &(_, i) in &order[..remove_n] {
            drop[i] = true;
        }
        let old = std::mem::take(&mut self.entries);
        let mut removed = Vec::with_capacity(remove_n);
        for (i, e) in old.into_iter().enumerate() {
            if drop[i] {
                removed.push(e);
            } else {
                self.entries.push(e);
            }
        }
        self.entries.shrink_to(self.max_entries + 16);
        self.rebuild();
        if let Some(conn) = &mut self.conn {
            let ok = (|| -> rusqlite::Result<()> {
                let tx = conn.transaction()?;
                {
                    let mut st = tx.prepare("DELETE FROM user_words WHERE code = ?1 AND word = ?2")?;
                    for e in &removed {
                        st.execute(params![code_to_string(dict, &e.code), e.text])?;
                    }
                }
                tx.commit()
            })();
            if ok.is_err() {
                self.write_errors += 1;
            }
        }
    }

    // ---- 单字 ----

    /// 单字的记忆加分（很小，要选几十次才会换位置）。
    pub fn char_bonus(&self, syl: u16, text: &str, now: i64) -> f32 {
        self.chars.get(&(syl, text.to_string())).map(|&(f, l)| char_bonus(effective(f, l, now))).unwrap_or(0.0)
    }

    /// 记一次单字的使用。
    pub fn learn_char(&mut self, dict: &Dict, syl: u16, text: &str, now: i64) {
        if text.chars().count() != 1 {
            return;
        }
        let e = self.chars.entry((syl, text.to_string())).or_insert((0.0, now));
        *e = (effective(e.0, e.1, now) + 1.0, now);
        let (f, l) = *e;
        if let Some(conn) = &self.conn {
            let r = conn.execute(
                "INSERT INTO single_chars(code, word, freq, last) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(code, word) DO UPDATE SET freq = excluded.freq, last = excluded.last",
                params![dict.syllable(syl), text, f, l],
            );
            if r.is_err() {
                self.write_errors += 1;
            }
        }
        if self.chars.len() > MAX_CHAR_ENTRIES {
            let mut v: Vec<((u16, String), f64)> =
                self.chars.iter().map(|(k, &(f, l))| (k.clone(), effective(f, l, now))).collect();
            v.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in v.into_iter().take(MAX_CHAR_ENTRIES / 10) {
                self.chars.remove(&k);
                if let Some(conn) = &self.conn {
                    let _ = conn.execute(
                        "DELETE FROM single_chars WHERE code = ?1 AND word = ?2",
                        params![dict.syllable(k.0), k.1],
                    );
                }
            }
        }
    }

    // ---- 打错纠正 ----

    /// 你接受过这种纠正的有效次数。
    pub fn fix_count(&self, typed: &str, meant: &str, now: i64) -> f64 {
        self.fixes.get(&(typed.to_string(), meant.to_string())).map(|&(f, l)| effective(f, l, now)).unwrap_or(0.0)
    }

    pub fn learn_fix(&mut self, typed: &str, meant: &str, now: i64) {
        let key = (typed.to_string(), meant.to_string());
        let e = self.fixes.entry(key).or_insert((0.0, now));
        *e = (effective(e.0, e.1, now) + 1.0, now);
        let (f, l) = *e;
        if let Some(conn) = &self.conn {
            let r = conn.execute(
                "INSERT INTO typo_fixes(typed, meant, freq, last) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(typed, meant) DO UPDATE SET freq = excluded.freq, last = excluded.last",
                params![typed, meant, f, l],
            );
            if r.is_err() {
                self.write_errors += 1;
            }
        }
        if self.fixes.len() > MAX_SMALL_ENTRIES {
            let mut v: Vec<((String, String), f64)> =
                self.fixes.iter().map(|(k, &(f, l))| (k.clone(), effective(f, l, now))).collect();
            v.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in v.into_iter().take(MAX_SMALL_ENTRIES / 10) {
                self.fixes.remove(&k);
                if let Some(conn) = &self.conn {
                    let _ = conn.execute("DELETE FROM typo_fixes WHERE typed = ?1 AND meant = ?2", params![k.0, k.1]);
                }
            }
        }
    }

    // ---- 英文词 ----

    pub fn learn_en(&mut self, text: &str, now: i64) {
        if text.len() < 2 || !text.bytes().all(|b| b.is_ascii_alphabetic()) {
            return;
        }
        let key = text.to_ascii_lowercase();
        let e = self.en.entry(key.clone()).or_insert(UEn { text: text.to_string(), freq: 0.0, last: now });
        e.freq = effective(e.freq, e.last, now) + 1.0;
        e.last = now;
        e.text = text.to_string();
        let (f, l) = (e.freq, e.last);
        if let Some(conn) = &self.conn {
            let r = conn.execute(
                "INSERT INTO english_words(key, word, freq, last) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(key) DO UPDATE SET word = excluded.word, freq = excluded.freq, last = excluded.last",
                params![key, text, f, l],
            );
            if r.is_err() {
                self.write_errors += 1;
            }
        }
        if self.en.len() > MAX_SMALL_ENTRIES {
            let mut v: Vec<(String, f64)> =
                self.en.iter().map(|(k, e)| (k.clone(), effective(e.freq, e.last, now))).collect();
            v.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in v.into_iter().take(MAX_SMALL_ENTRIES / 10) {
                self.en.remove(&k);
                if let Some(conn) = &self.conn {
                    let _ = conn.execute("DELETE FROM english_words WHERE key = ?1", params![k]);
                }
            }
        }
    }

    /// 记住的英文词（小写键 → 记录）。
    pub fn english(&self) -> &HashMap<String, UEn> {
        &self.en
    }

    // ---- 首选被冷落 ----

    /// 本来排第一的词，你却选了别的：记一次冷落。冷落满 3 次就删掉它。返回是否已删掉。
    pub fn reject(&mut self, dict: &Dict, code: &[u16], text: &str) -> bool {
        if code.is_empty() || text.chars().count() < 2 || self.is_blocked(code, text) {
            return false;
        }
        let key = (code.to_vec(), text.to_string());
        let n = {
            let e = self.rejects.entry(key.clone()).or_insert(0);
            *e += 1;
            *e
        };
        if n >= REJECT_LIMIT {
            self.rejects.remove(&key);
            if let Some(conn) = &self.conn {
                let _ = conn.execute(
                    "DELETE FROM rejected WHERE code = ?1 AND word = ?2",
                    params![code_to_string(dict, code), text],
                );
            }
            self.forget(dict, code, text);
            return true;
        }
        if let Some(conn) = &self.conn {
            let r = conn.execute(
                "INSERT INTO rejected(code, word, count) VALUES (?1, ?2, ?3)
                 ON CONFLICT(code, word) DO UPDATE SET count = excluded.count",
                params![code_to_string(dict, code), text, n],
            );
            if r.is_err() {
                self.write_errors += 1;
            }
        }
        if self.rejects.len() > MAX_SMALL_ENTRIES {
            // 太多了就整个清空（冷落记录不重要）
            self.rejects.clear();
            if let Some(conn) = &self.conn {
                let _ = conn.execute("DELETE FROM rejected", []);
            }
        }
        false
    }

    pub fn reject_count(&self, code: &[u16], text: &str) -> u32 {
        self.rejects.get(&(code.to_vec(), text.to_string())).copied().unwrap_or(0)
    }

    // ---- 查词时用的字典树 ----

    pub(crate) fn children_in(&self, node: u32, lo: u16, hi: u16) -> &[(u16, u32)] {
        let c = &self.nodes[node as usize].children;
        let a = c.partition_point(|x| x.0 < lo);
        let b = c.partition_point(|x| x.0 < hi);
        &c[a..b.max(a)]
    }

    pub(crate) fn node_entries(&self, node: u32) -> &[u32] {
        &self.nodes[node as usize].entries
    }

    pub fn entry(&self, i: u32) -> &UEntry {
        &self.entries[i as usize]
    }

    pub fn entries(&self) -> &[UEntry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictBuilder;

    fn dict() -> Dict {
        let mut b = DictBuilder::new(["zhang", "zi", "han", "shi"].map(String::from));
        b.add(&[0], "张", -5.0);
        Dict::from_bytes(b.build()).unwrap()
    }

    #[test]
    fn learns_words_but_not_single_chars() {
        let d = dict();
        let mut u = UserDb::in_memory();
        let code = vec![d.syllable_id("zhang").unwrap(), d.syllable_id("zi").unwrap(), d.syllable_id("han").unwrap()];
        u.learn(&d, &code, "张子涵", 1000);
        u.learn(&d, &[d.syllable_id("shi").unwrap()], "事", 1000);
        assert_eq!(u.len(), 1);
        assert!(u.bonus(&code, "张子涵", 1000) > 20.0);
        // 一年后加分明显变小，但还在
        let later = u.bonus(&code, "张子涵", 1000 + 365 * 86400);
        assert!(later > 0.0 && later < 5.0, "{later}");
    }

    #[test]
    fn persists_and_reloads() {
        let d = dict();
        let dir = std::env::temp_dir().join(format!("pyime-test-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user.db");
        let code = vec![d.syllable_id("zi").unwrap(), d.syllable_id("han").unwrap()];
        {
            let mut u = UserDb::open(&path, &d).unwrap();
            u.learn(&d, &code, "子涵", 50);
            u.learn(&d, &code, "子涵", 60);
            u.forget(&d, &code, "紫涵");
        }
        let u = UserDb::open(&path, &d).unwrap();
        assert_eq!(u.len(), 1);
        assert!(u.lookup(&code, "子涵").unwrap().freq > 1.9);
        assert!(u.is_blocked(&code, "紫涵"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejected_three_times_is_deleted() {
        let d = dict();
        let mut u = UserDb::in_memory();
        let code = vec![d.syllable_id("zi").unwrap(), d.syllable_id("han").unwrap()];
        assert!(!u.reject(&d, &code, "紫涵"));
        assert!(!u.reject(&d, &code, "紫涵"));
        // 中间选了它一次，冷落清零
        u.learn(&d, &code, "紫涵", 0);
        assert_eq!(u.reject_count(&code, "紫涵"), 0);
        u.forget(&d, &code, "紫涵");
        let code2 = vec![d.syllable_id("zhang").unwrap(), d.syllable_id("zi").unwrap()];
        assert!(!u.reject(&d, &code2, "长子"));
        assert!(!u.reject(&d, &code2, "长子"));
        assert!(u.reject(&d, &code2, "长子"));
        assert!(u.is_blocked(&code2, "长子"));
    }

    #[test]
    fn remembers_fixes_and_english() {
        let d = dict();
        let dir = std::env::temp_dir().join(format!("pyime-test-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user.db");
        {
            let mut u = UserDb::open(&path, &d).unwrap();
            u.learn_fix("u", "i", 100);
            u.learn_fix("u", "i", 100);
            u.learn_en("Kubernetes", 100);
            u.learn_en("x", 100);
        }
        let u = UserDb::open(&path, &d).unwrap();
        assert!(u.fix_count("u", "i", 100) > 1.9);
        assert_eq!(u.english().get("kubernetes").unwrap().text, "Kubernetes");
        assert_eq!(u.english().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eviction_keeps_size_bounded() {
        let d = dict();
        let mut u = UserDb::in_memory();
        u.set_max_entries(100);
        let z = d.syllable_id("zi").unwrap();
        for i in 0..1000u32 {
            let text = format!("词{i}");
            u.learn(&d, &[z, z], &text, i as i64);
            assert!(u.len() <= 100);
        }
        // 留下的是最近用的
        assert!(u.lookup(&[z, z], "词999").is_some());
        assert!(u.lookup(&[z, z], "词0").is_none());
    }
}
