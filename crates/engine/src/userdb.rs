//! 用户词库：记住你选过的词和自己拼出来的词。
//!
//! - 只记两个字以上的词，单字不调频。
//! - 每个词记"有效次数"和最后使用时间；有效次数随时间按半衰期衰减，所以很久不用的词会慢慢退回去。
//! - 条数有上限，超过后淘汰有效次数最低的，内存和文件都不会无限增长。
//! - 存在本机 SQLite 文件里（WAL 模式，断电也不会损坏）；不保存任何打字记录。

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection};

use crate::dict::Dict;

/// 半衰期：90 天不用，分量减半。
pub const HALF_LIFE_SECS: f64 = 90.0 * 86400.0;
/// 默认最多记 10 万条。
pub const DEFAULT_MAX_ENTRIES: usize = 100_000;

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
