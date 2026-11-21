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
/// 搭配（上一个词 → 你选的字词）最多记这么多条。
pub const MAX_PAIR_ENTRIES: usize = 20_000;
/// 人名最多记这么多条（人名不随时间变淡、记忆库满了也不删，只有超过这个数才删最少用的）。
pub const MAX_NAMES: usize = 20_000;

const DAY: i64 = 86400;
/// 分几次选拼起来的短语（不是人名、不是你加的词）：只用过一次，这么久没再用就删
pub const PHRASE_TTL: i64 = 180 * DAY;
/// 搭配：这么久没再出现就删
pub const PAIR_TTL: i64 = 365 * DAY;
/// 首选被冷落：这么久内没凑满 3 次，计数清零
pub const REJECT_WINDOW: i64 = 30 * DAY;
/// 拒绝过的纠错：这么久以后忘掉
pub const FIX_REJECT_TTL: i64 = 365 * DAY;

/// 常见姓氏（单姓 + 复姓），用来认出人名
const SURNAMES: &str = "王李张刘陈杨黄赵吴周徐孙马朱胡郭何高林罗郑梁谢宋唐许韩冯邓曹彭曾肖田董袁潘于蒋蔡余杜叶程苏魏吕丁任沈姚卢姜崔钟谭陆汪范金石廖贾夏韦付方白邹孟熊秦邱江尹薛闫段雷侯龙史陶黎贺顾毛郝龚邵万钱严覃武戴莫孔向汤常温康施文牛樊葛邢安齐易乔伍庞颜倪庄聂章鲁岳翟殷詹申欧耿关兰焦俞左柳甘祝包宁尚符舒阮柯纪梅童凌毕单季裴霍涂成苗谷盛曲翁冉骆蓝路游辛靳管柴蒙鲍华喻祁蒲房滕屈饶解牟艾尤阳时穆农司卓古吉缪简车项连芦麦褚娄窦戚岑景党宫费卜冷晏席卫米柏宗瞿桂全佟应臧闵苟邬边卞姬师和仇栾隋商刁沙荣巫寇桑郎甄丛仲虞敖巩明佘池查麻苑迟邝官封谈匡鞠惠荆乐冀郁胥南班储原栗燕楚鄢劳谌奚皮粟冼蔺楼盘满闻位厉伊仝区郜海阚花权强帅屠豆朴盖练廉禹井祖漆巴丰支卿国狄平计索宣晋相初门云容敬来扈晁芮都普阙浦戈伏鹿薄邸雍辜羊阿乌母裘亓修邰赫杭况那宿鲜印逯隆茹诸战慕危玉银亢嵇公哈湛宾戎勾茅利於呼居揭干但尉冶斯元束檀衣信展阴昝智幸奉植衡富尧闭由";
const COMPOUND_SURNAMES: [&str; 16] = [
    "欧阳", "司马", "诸葛", "上官", "东方", "皇甫", "尉迟", "令狐", "慕容", "长孙", "夏侯", "司徒", "公孙", "端木",
    "宇文", "南宫",
];

/// 虚词：第二个字是它的不会是人名（马的、王了）
const PARTICLES: &str = "的了着过吗呢吧啊么也都就还在是有和与把被给对说";

/// 是人名：词库里没有，看起来像人名，而且前两个字不是一个现成的词（高兴地、张开嘴、时间到 不是人名）。
pub fn is_name(dict: &Dict, code: &[u16], text: &str) -> bool {
    if !looks_like_name(text) || dict.has_word(code, text) {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    if PARTICLES.contains(chars[1]) {
        return false;
    }
    if chars.len() >= 3 && code.len() == chars.len() {
        let head: String = chars[..2].iter().collect();
        if dict.has_word(&code[..2], &head) && !COMPOUND_SURNAMES.contains(&head.as_str()) {
            return false;
        }
    }
    true
}

/// 看起来像人名：2–4 个汉字，以常见姓氏开头。
pub fn looks_like_name(text: &str) -> bool {
    let n = text.chars().count();
    if !(2..=4).contains(&n) || !text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return false;
    }
    if COMPOUND_SURNAMES.iter().any(|s| text.starts_with(s)) {
        return n >= 3;
    }
    text.chars().next().is_some_and(|c| SURNAMES.contains(c))
}

/// 搭配的记忆加分：在同样的上文后面选过一次就够（约 +2.3），比上文本身的影响大，
/// 所以"上文让别的字排第一、你却选了这个"之后，下次同样的上文就是这个。
pub fn pair_bonus(eff: f64) -> f32 {
    (1.5 * eff.min(1.0) + 1.2 * (1.0 + eff).ln()) as f32
}

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
    /// 人名（词库里没有、以常见姓氏开头）：不随时间变淡，记忆库满了也不删
    pub name: bool,
    /// 你在设置页加入的词：还没用过时排第二个
    pub added: bool,
}

impl UEntry {
    pub fn effective(&self, now: i64) -> f64 {
        if self.name {
            self.freq
        } else {
            effective(self.freq, self.last, now)
        }
    }
}

pub fn effective(freq: f64, last: i64, now: i64) -> f64 {
    let dt = (now - last).max(0) as f64;
    freq * 0.5f64.powf(dt / HALF_LIFE_SECS)
}

/// 词的记忆加分：选一次约 +3.5，三次约 +5，十次约 +7.3；随时间衰减。
/// 一次就足够让人名、菜名这种没有常用竞争对手的词排第一；
/// 但偶尔选一次的冷门词（世情、是青）不会挤掉很常用的词（事情），而是放进前 4 个（见 lattice.rs 的"上次选过的"）；
/// 选三次左右才排第一。
pub fn bonus(eff: f64) -> f32 {
    (2.0 * eff.min(1.0) + 2.2 * (1.0 + eff).ln()) as f32
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
    /// 首选被冷落：(次数, 最后一次的时间)
    rejects: HashMap<(Vec<u16>, String), (u32, i64)>,
    /// 拒绝过的纠错：(你打的字母, 纠错出来的字词) → 时间
    fix_rejects: HashMap<(String, String), i64>,
    /// 上次按时间清理的时间
    last_maintain: i64,
    /// 单字：(音节, 字) → (有效次数, 最后时间)
    chars: HashMap<(u16, String), (f64, i64)>,
    /// 搭配：(上一个词, 这个字词) → (有效次数, 最后时间)
    pairs: HashMap<(String, String), (f64, i64)>,
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
            fix_rejects: HashMap::new(),
            last_maintain: 0,
            chars: HashMap::new(),
            pairs: HashMap::new(),
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
                 PRIMARY KEY(code, word)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS user_pairs(
                 prev TEXT NOT NULL, word TEXT NOT NULL,
                 freq REAL NOT NULL, last INTEGER NOT NULL,
                 PRIMARY KEY(prev, word)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS fix_rejects(
                 typed TEXT NOT NULL, word TEXT NOT NULL, last INTEGER NOT NULL,
                 PRIMARY KEY(typed, word)) WITHOUT ROWID;",
        )
        .map_err(|e| format!("初始化用户词库失败：{e}"))?;
        // 旧版本的表补上新列（已经有了会报错，不用管）
        let _ = conn.execute("ALTER TABLE user_words ADD COLUMN added INTEGER NOT NULL DEFAULT 0", []);
        let _ = conn.execute("ALTER TABLE rejected ADD COLUMN last INTEGER NOT NULL DEFAULT 0", []);

        let mut db = UserDb::in_memory();
        {
            let mut st =
                conn.prepare("SELECT code, word, freq, last, added FROM user_words").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)? != 0,
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows.flatten() {
                let (code, text, freq, last, added) = row;
                // 词库升级后不认识的拼音直接跳过
                if let Some(code) = code_from_string(dict, &code) {
                    if text.chars().count() >= 2 && freq.is_finite() && (freq > 0.0 || added && freq >= 0.0) {
                        let name = is_name(dict, &code, &text);
                        db.entries.push(UEntry { code, text, freq, last, name, added });
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
            let mut st = conn.prepare("SELECT code, word, count, last FROM rejected").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?, r.get::<_, i64>(3)?))
                })
                .map_err(|e| e.to_string())?;
            for (code, text, n, last) in rows.flatten() {
                if let Some(code) = code_from_string(dict, &code) {
                    db.rejects.insert((code, text), (n, last));
                }
            }
            let mut st = conn.prepare("SELECT typed, word, last FROM fix_rejects").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
                .map_err(|e| e.to_string())?;
            for (a, b, t) in rows.flatten() {
                db.fix_rejects.insert((a, b), t);
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
            let mut st = conn.prepare("SELECT prev, word, freq, last FROM user_pairs").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?))
                })
                .map_err(|e| e.to_string())?;
            for (a, b, f, l) in rows.flatten() {
                if f.is_finite() && f > 0.0 {
                    db.pairs.insert((a, b), (f, l));
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
                let name = is_name(dict, code, text);
                self.entries.push(UEntry {
                    code: code.to_vec(),
                    text: text.to_string(),
                    freq: 1.0,
                    last: now,
                    name,
                    added: false,
                });
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
        // 人名不删（除非人名本身多得超过上限）；其余按"有效次数"从少到多删
        // 人名最多占容量的一半（正常用不完；保证记忆库不会无限增长）
        let names = self.entries.iter().filter(|e| e.name).count();
        let protect = names <= MAX_NAMES.min(self.max_entries / 2);
        let mut order: Vec<(f64, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !(protect && e.name))
            .map(|(i, e)| (e.effective(now), i))
            .collect();
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        let remove_n = (self.entries.len() - keep).min(order.len());
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

    /// 上次选这个字（词）是什么时候。
    pub fn last_pick(&self, code: &[u16], text: &str) -> Option<i64> {
        if code.len() == 1 && text.chars().count() == 1 {
            self.chars.get(&(code[0], text.to_string())).map(|x| x.1)
        } else {
            self.lookup(code, text).map(|e| e.last)
        }
    }

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

    /// 单字用过的有效次数。
    pub fn char_count(&self, syl: u16, text: &str, now: i64) -> f64 {
        self.chars.get(&(syl, text.to_string())).map(|&(f, l)| effective(f, l, now)).unwrap_or(0.0)
    }

    // ---- 搭配（前后词关联） ----

    /// 上一个词是 `prev` 时，你选过 `text` 的记忆加分。
    pub fn pair_bonus(&self, prev: &str, text: &str, now: i64) -> f32 {
        self.pairs
            .get(&(prev.to_string(), text.to_string()))
            .map(|&(f, l)| pair_bonus(effective(f, l, now)))
            .unwrap_or(0.0)
    }

    /// 记一次"上一个词是 `prev` 时选了 `text`"。
    pub fn learn_pair(&mut self, prev: &str, text: &str, now: i64) {
        if prev.is_empty() || text.is_empty() || prev.chars().count() > 8 || text.chars().count() > 8 {
            return;
        }
        let e = self.pairs.entry((prev.to_string(), text.to_string())).or_insert((0.0, now));
        *e = (effective(e.0, e.1, now) + 1.0, now);
        let (f, l) = *e;
        if let Some(conn) = &self.conn {
            let r = conn.execute(
                "INSERT INTO user_pairs(prev, word, freq, last) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(prev, word) DO UPDATE SET freq = excluded.freq, last = excluded.last",
                params![prev, text, f, l],
            );
            if r.is_err() {
                self.write_errors += 1;
            }
        }
        if self.pairs.len() > MAX_PAIR_ENTRIES {
            // 满了：淘汰最不常用、最久没用的一成
            let mut v: Vec<((String, String), f64)> =
                self.pairs.iter().map(|(k, &(f, l))| (k.clone(), effective(f, l, now))).collect();
            v.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in v.into_iter().take(MAX_PAIR_ENTRIES / 10) {
                self.pairs.remove(&k);
                if let Some(conn) = &self.conn {
                    let _ = conn.execute("DELETE FROM user_pairs WHERE prev = ?1 AND word = ?2", params![k.0, k.1]);
                }
            }
        }
    }

    pub fn pair_len(&self) -> usize {
        self.pairs.len()
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
    pub fn reject(&mut self, dict: &Dict, code: &[u16], text: &str, now: i64) -> bool {
        if code.is_empty() || text.chars().count() < 2 || self.is_blocked(code, text) {
            return false;
        }
        let key = (code.to_vec(), text.to_string());
        let n = {
            let e = self.rejects.entry(key.clone()).or_insert((0, now));
            // 一个月内没凑满 3 次：之前的不算了（偶尔一两次没选它，不该一直记着）
            if now - e.1 > REJECT_WINDOW {
                e.0 = 0;
            }
            e.0 += 1;
            e.1 = now;
            e.0
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
                "INSERT INTO rejected(code, word, count, last) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(code, word) DO UPDATE SET count = excluded.count, last = excluded.last",
                params![code_to_string(dict, code), text, n, now],
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
        self.rejects.get(&(code.to_vec(), text.to_string())).map(|x| x.0).unwrap_or(0)
    }

    // ---- 拒绝过的纠错 ----

    /// 你打 `typed`，纠错出来的 `text` 排在前面，你却选了原样拼写的：以后这个纠错不再排前面。
    pub fn reject_fix(&mut self, typed: &str, text: &str, now: i64) {
        if typed.is_empty() || text.is_empty() {
            return;
        }
        self.fix_rejects.insert((typed.to_string(), text.to_string()), now);
        if let Some(conn) = &self.conn {
            let r = conn.execute(
                "INSERT INTO fix_rejects(typed, word, last) VALUES (?1, ?2, ?3)
                 ON CONFLICT(typed, word) DO UPDATE SET last = excluded.last",
                params![typed, text, now],
            );
            if r.is_err() {
                self.write_errors += 1;
            }
        }
        if self.fix_rejects.len() > MAX_SMALL_ENTRIES {
            let mut v: Vec<((String, String), i64)> = self.fix_rejects.iter().map(|(k, &t)| (k.clone(), t)).collect();
            v.sort_by_key(|x| x.1);
            for (k, _) in v.into_iter().take(MAX_SMALL_ENTRIES / 10) {
                self.fix_rejects.remove(&k);
                if let Some(conn) = &self.conn {
                    let _ = conn.execute("DELETE FROM fix_rejects WHERE typed = ?1 AND word = ?2", params![k.0, k.1]);
                }
            }
        }
    }

    pub fn fix_rejected(&self, typed: &str, text: &str) -> bool {
        self.fix_rejects.contains_key(&(typed.to_string(), text.to_string()))
    }

    // ---- 你在设置页加入的词 ----

    /// 加入一个词：还没用过时排第二个，以后和别的词一样（用得多排第一，久不用慢慢往后）。
    /// 返回是不是新加的（已经有了返回 false）。
    pub fn add_word(&mut self, dict: &Dict, code: &[u16], text: &str, now: i64) -> bool {
        if code.is_empty() || text.chars().count() < 2 {
            return false;
        }
        // 以前删掉过的词，你主动加回来：取消删除
        if let Some(v) = self.blocked.get_mut(text) {
            v.retain(|c| c != code);
            if let Some(conn) = &self.conn {
                let _ = conn.execute(
                    "DELETE FROM blocked_words WHERE code = ?1 AND word = ?2",
                    params![code_to_string(dict, code), text],
                );
            }
        }
        self.rejects.remove(&(code.to_vec(), text.to_string()));
        let new = match self.find(code, text) {
            Some(i) => {
                let e = &mut self.entries[i as usize];
                let was = e.added;
                e.added = true;
                !was
            }
            None => {
                let name = is_name(dict, code, text);
                self.entries.push(UEntry {
                    code: code.to_vec(),
                    text: text.to_string(),
                    freq: 0.0,
                    last: now,
                    name,
                    added: true,
                });
                self.index((self.entries.len() - 1) as u32);
                true
            }
        };
        if let Some(conn) = &self.conn {
            let r = conn.execute(
                "INSERT INTO user_words(code, word, freq, last, added) VALUES (?1, ?2, 0, ?3, 1)
                 ON CONFLICT(code, word) DO UPDATE SET added = 1",
                params![code_to_string(dict, code), text, now],
            );
            if r.is_err() {
                self.write_errors += 1;
            }
        }
        if self.entries.len() > self.max_entries {
            self.evict(dict, now);
        }
        new
    }

    // ---- 按时间清理（只有个别几类） ----

    /// 一天最多清理一次。
    pub fn maybe_maintain(&mut self, dict: &Dict, now: i64) {
        if now - self.last_maintain >= DAY {
            self.maintain(dict, now);
        }
    }

    /// 按时间删除：
    /// - 分几次选拼起来的短语（词库里没有、不是人名、不是你加的）：只用过一次、半年没再用
    /// - 搭配：一年没再出现
    /// - 拒绝过的纠错：一年以后忘掉
    ///
    /// 人名、你选过的词库里的词、你加的词、单字习惯都不按时间删（它们只会慢慢往后排）。
    pub fn maintain(&mut self, dict: &Dict, now: i64) {
        self.last_maintain = now;
        let stale: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                !e.name && !e.added && e.freq <= 1.0 && now - e.last > PHRASE_TTL && !dict.has_word(&e.code, &e.text)
            })
            .map(|(i, _)| i)
            .collect();
        if !stale.is_empty() {
            let mut removed = Vec::new();
            for &i in stale.iter().rev() {
                removed.push(self.entries.swap_remove(i));
            }
            self.rebuild();
            if let Some(conn) = &self.conn {
                for e in &removed {
                    let _ = conn.execute(
                        "DELETE FROM user_words WHERE code = ?1 AND word = ?2",
                        params![code_to_string(dict, &e.code), e.text],
                    );
                }
            }
        }
        let old_pairs: Vec<(String, String)> =
            self.pairs.iter().filter(|(_, &(_, l))| now - l > PAIR_TTL).map(|(k, _)| k.clone()).collect();
        for k in old_pairs {
            self.pairs.remove(&k);
            if let Some(conn) = &self.conn {
                let _ = conn.execute("DELETE FROM user_pairs WHERE prev = ?1 AND word = ?2", params![k.0, k.1]);
            }
        }
        let old_fix: Vec<(String, String)> =
            self.fix_rejects.iter().filter(|(_, &t)| now - t > FIX_REJECT_TTL).map(|(k, _)| k.clone()).collect();
        for k in old_fix {
            self.fix_rejects.remove(&k);
            if let Some(conn) = &self.conn {
                let _ = conn.execute("DELETE FROM fix_rejects WHERE typed = ?1 AND word = ?2", params![k.0, k.1]);
            }
        }
        let old_rej: Vec<(Vec<u16>, String)> =
            self.rejects.iter().filter(|(_, &(_, l))| now - l > REJECT_WINDOW).map(|(k, _)| k.clone()).collect();
        for k in old_rej {
            self.rejects.remove(&k);
            if let Some(conn) = &self.conn {
                let _ = conn.execute(
                    "DELETE FROM rejected WHERE code = ?1 AND word = ?2",
                    params![code_to_string(dict, &k.0), k.1],
                );
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
        assert!(u.bonus(&code, "张子涵", 1000) > 3.0);
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
    fn pairs_persist_and_stay_bounded() {
        let d = dict();
        let dir = std::env::temp_dir().join(format!("pyime-test-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user.db");
        {
            let mut u = UserDb::open(&path, &d).unwrap();
            assert_eq!(u.pair_bonus("滚", "滚", 10), 0.0);
            u.learn_pair("滚", "滚", 10);
            // 选一次就比上文本身的影响大
            assert!(u.pair_bonus("滚", "滚", 10) > 2.0);
        }
        let mut u = UserDb::open(&path, &d).unwrap();
        assert!(u.pair_bonus("滚", "滚", 10) > 2.0, "重新打开后还记得");
        // 再多也不超过上限
        for i in 0..(MAX_PAIR_ENTRIES + 50) {
            u.learn_pair(&format!("词{i}"), "字", 20);
        }
        assert!(u.pair_len() <= MAX_PAIR_ENTRIES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_recognised() {
        let d = dict();
        let zz = |a: &str, b: &str| vec![d.syllable_id(a).unwrap(), d.syllable_id(b).unwrap()];
        assert!(is_name(&d, &zz("zhang", "zi"), "张子"));
        assert!(looks_like_name("李卓轩"));
        assert!(looks_like_name("欧阳娜娜"));
        assert!(!looks_like_name("欧阳"), "单独的复姓不是名字");
        assert!(!looks_like_name("不知道"), "不以姓氏开头");
        assert!(!looks_like_name("abc"));
        assert!(!is_name(&d, &zz("shi", "zi"), "马的"), "第二个字是虚词");
    }

    #[test]
    fn names_never_fade_or_get_evicted() {
        let d = dict();
        let mut u = UserDb::in_memory();
        u.set_max_entries(20);
        let name = vec![d.syllable_id("zhang").unwrap(), d.syllable_id("zi").unwrap(), d.syllable_id("han").unwrap()];
        u.learn(&d, &name, "张子涵", 0);
        let year = 365 * DAY;
        assert!(u.lookup(&name, "张子涵").unwrap().name);
        // 三年不用，记忆分量不变
        assert_eq!(u.lookup(&name, "张子涵").unwrap().effective(3 * year), 1.0);
        // 记忆库满了也不删
        for i in 0..100 {
            let c = vec![d.syllable_id("shi").unwrap(), d.syllable_id("zi").unwrap()];
            u.learn(&d, &c, &format!("词{i}"), 3 * year + i);
        }
        assert!(u.len() <= 20);
        assert!(u.lookup(&name, "张子涵").is_some(), "人名不该被挤掉");
        // 按时间清理也不删人名
        u.maintain(&d, 10 * year);
        assert!(u.lookup(&name, "张子涵").is_some());
    }

    #[test]
    fn only_a_few_kinds_are_deleted_by_time() {
        let d = dict();
        let mut u = UserDb::in_memory();
        let year = 365 * DAY;
        let c = vec![d.syllable_id("shi").unwrap(), d.syllable_id("zi").unwrap()];
        // 拼起来的短语只用过一次：半年后删
        u.learn(&d, &c, "是子了吗", 0);
        // 用过几次的：不按时间删（只会慢慢往后排）
        u.learn(&d, &c, "是子", 0);
        u.learn(&d, &c, "是子", 1);
        // 你加的词：不按时间删
        u.add_word(&d, &c, "试资", 0);
        // 搭配：一年后删
        u.learn_pair("我", "是", 0);
        u.maintain(&d, 100 * DAY);
        assert!(u.lookup(&c, "是子了吗").is_some(), "100 天还不删");
        u.maintain(&d, 200 * DAY);
        assert!(u.lookup(&c, "是子了吗").is_none(), "只用过一次、半年没用：删掉");
        assert!(u.lookup(&c, "是子").is_some());
        assert!(u.lookup(&c, "试资").is_some());
        assert!(u.pair_bonus("我", "是", 200 * DAY) > 0.0);
        u.maintain(&d, 2 * year);
        assert_eq!(u.pair_bonus("我", "是", 2 * year), 0.0, "一年没再出现的搭配删掉");
        assert!(u.lookup(&c, "是子").is_some());
        assert!(u.lookup(&c, "试资").is_some());
    }

    #[test]
    fn rejections_reset_after_a_month() {
        let d = dict();
        let mut u = UserDb::in_memory();
        let c = vec![d.syllable_id("shi").unwrap(), d.syllable_id("zi").unwrap()];
        assert!(!u.reject(&d, &c, "柿子", 0));
        assert!(!u.reject(&d, &c, "柿子", DAY));
        // 隔了两个月才第三次：之前两次不算
        assert!(!u.reject(&d, &c, "柿子", 60 * DAY));
        assert_eq!(u.reject_count(&c, "柿子"), 1);
        assert!(!u.is_blocked(&c, "柿子"));
    }

    #[test]
    fn added_words_start_unused_and_survive_reload() {
        let d = dict();
        let dir = std::env::temp_dir().join(format!("pyime-test-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user.db");
        let c = vec![d.syllable_id("shi").unwrap(), d.syllable_id("zi").unwrap()];
        {
            let mut u = UserDb::open(&path, &d).unwrap();
            assert!(u.add_word(&d, &c, "柿子", 10));
            assert!(!u.add_word(&d, &c, "柿子", 10), "加两次只算一次");
            let e = u.lookup(&c, "柿子").unwrap();
            assert!(e.added && e.freq == 0.0);
        }
        let mut u = UserDb::open(&path, &d).unwrap();
        let e = u.lookup(&c, "柿子").unwrap();
        assert!(e.added, "重新打开后还在");
        // 用过以后和别的词一样
        u.learn(&d, &c, "柿子", 20);
        assert!(u.lookup(&c, "柿子").unwrap().freq >= 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejected_three_times_is_deleted() {
        let d = dict();
        let mut u = UserDb::in_memory();
        let code = vec![d.syllable_id("zi").unwrap(), d.syllable_id("han").unwrap()];
        assert!(!u.reject(&d, &code, "紫涵", 100));
        assert!(!u.reject(&d, &code, "紫涵", 100));
        // 中间选了它一次，冷落清零
        u.learn(&d, &code, "紫涵", 0);
        assert_eq!(u.reject_count(&code, "紫涵"), 0);
        u.forget(&d, &code, "紫涵");
        let code2 = vec![d.syllable_id("zhang").unwrap(), d.syllable_id("zi").unwrap()];
        assert!(!u.reject(&d, &code2, "长子", 100));
        assert!(!u.reject(&d, &code2, "长子", 100));
        assert!(u.reject(&d, &code2, "长子", 100));
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
