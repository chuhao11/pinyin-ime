//! 输入法（每个程序里的 DLL）和引擎进程之间的通信协议，以及引擎进程里和平台无关的处理逻辑。
//!
//! - 每条消息：4 字节长度（小端）+ 内容。内容第一个字节是消息类型。
//! - 一个连接对应一个程序（一个 `Session`）；所有连接共用一个 `Engine`。
//! - 两边收到的消息都做完整的越界检查：引擎不信任 DLL，DLL 也不信任引擎。
//!
//! 只在本机、只在本人账户下通信（Windows 的命名管道权限见 winsrv）。

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use pinyin_types::{Key, Mode, Mods, View};

pub use pinyin_types;

/// 协议版本：两边不一致时 DLL 不用引擎（原样放过按键），不会出怪事。
/// 2：候选窗多了"按了一下 `，再按就删"的标记
/// 3：隐私输入框（SetPrivate）
pub const PROTO: u32 = 3;
/// 一条消息最长多少字节。
pub const MAX_FRAME: usize = 256 * 1024;
const MAX_STR: usize = 16 * 1024;
const MAX_ITEMS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Hello {
        proto: u32,
        pid: u32,
    },
    /// 处理一个按键
    Key {
        key: Key,
        mods: Mods,
    },
    /// 只问"这个键要不要"，不改变状态
    TestKey {
        key: Key,
        mods: Mods,
    },
    /// 清空正在打的拼音（输入框失去焦点、程序把输入打断了）
    Reset,
    /// 忘掉上文（光标被鼠标移走了）
    ClearContext,
    GetState,
    /// 设置改了，重新读
    ReloadSettings,
    SetEnglishMode {
        on: bool,
    },
    /// 设置页里加入词语：一段文字，词语之间用任意标点、空格、换行隔开；
    /// 多音字可以写成 词语(pin yin)
    AddWords {
        text: String,
    },
    /// 现在的输入框是不是隐私输入框（浏览器无痕窗口等）：是的话照常打字，但什么都不记
    SetPrivate {
        on: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Hello {
        proto: u32,
    },
    KeyResult {
        consumed: bool,
        commit: Option<String>,
        view: View,
    },
    Bool(bool),
    Ok,
    State {
        english_mode: bool,
        chinese_punct: bool,
    },
    /// 加入词语的结果：加了几个，跳过的（和原因）
    WordsAdded {
        added: u32,
        skipped: Vec<String>,
    },
    Error(String),
}

// ---------------- 编码 ----------------

struct Enc(Vec<u8>);

impl Enc {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bool(&mut self, v: bool) {
        self.0.push(v as u8);
    }
    fn str(&mut self, s: &str) {
        let b = &s.as_bytes()[..s.len().min(MAX_STR)];
        // 截断时不能截在字符中间
        let mut n = b.len();
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        self.u32(n as u32);
        self.0.extend_from_slice(&s.as_bytes()[..n]);
    }
    fn key(&mut self, k: Key) {
        match k {
            Key::Char(c) => {
                self.u8(0);
                self.u32(c as u32);
            }
            Key::Space => self.u8(1),
            Key::Enter => self.u8(2),
            Key::Backspace => self.u8(3),
            Key::Escape => self.u8(4),
            Key::Left => self.u8(5),
            Key::Right => self.u8(6),
            Key::Up => self.u8(7),
            Key::Down => self.u8(8),
            Key::Other => self.u8(9),
            Key::ShiftTap => self.u8(10),
        }
    }
    fn mods(&mut self, m: Mods) {
        self.u8((m.ctrl as u8) | (m.alt as u8) << 1 | (m.caps_lock as u8) << 2);
    }
    fn view(&mut self, v: &View) {
        self.bool(v.composing);
        self.str(&v.preedit);
        self.u8(match v.mode {
            Mode::Row => 0,
            Mode::Grid => 1,
        });
        let n = v.items.len().min(MAX_ITEMS);
        self.u32(n as u32);
        for i in 0..n {
            self.str(&v.items[i]);
            self.bool(v.corrected.get(i).copied().unwrap_or(false));
        }
        self.u32(v.highlight as u32);
        self.u32(v.page as u32);
        self.u32(v.pages as u32);
        self.u32(v.total as u32);
        self.bool(v.english_mode);
        self.bool(v.armed);
    }
}

#[derive(Debug)]
pub struct DecodeError(pub &'static str);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "消息格式不对：{}", self.0)
    }
}

impl std::error::Error for DecodeError {}

struct Dec<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.p.checked_add(n).ok_or(DecodeError("长度溢出"))?;
        let s = self.b.get(self.p..end).ok_or(DecodeError("消息太短"))?;
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn bool(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError("布尔值不对")),
        }
    }
    fn str(&mut self) -> Result<String, DecodeError> {
        let n = self.u32()? as usize;
        if n > MAX_STR {
            return Err(DecodeError("文字太长"));
        }
        let s = self.take(n)?;
        String::from_utf8(s.to_vec()).map_err(|_| DecodeError("文字不是 UTF-8"))
    }
    fn key(&mut self) -> Result<Key, DecodeError> {
        Ok(match self.u8()? {
            0 => Key::Char(char::from_u32(self.u32()?).ok_or(DecodeError("字符不对"))?),
            1 => Key::Space,
            2 => Key::Enter,
            3 => Key::Backspace,
            4 => Key::Escape,
            5 => Key::Left,
            6 => Key::Right,
            7 => Key::Up,
            8 => Key::Down,
            9 => Key::Other,
            10 => Key::ShiftTap,
            _ => return Err(DecodeError("按键类型不对")),
        })
    }
    fn mods(&mut self) -> Result<Mods, DecodeError> {
        let m = self.u8()?;
        if m > 7 {
            return Err(DecodeError("修饰键不对"));
        }
        Ok(Mods { ctrl: m & 1 != 0, alt: m & 2 != 0, caps_lock: m & 4 != 0 })
    }
    fn view(&mut self) -> Result<View, DecodeError> {
        let composing = self.bool()?;
        let preedit = self.str()?;
        let mode = match self.u8()? {
            0 => Mode::Row,
            1 => Mode::Grid,
            _ => return Err(DecodeError("模式不对")),
        };
        let n = self.u32()? as usize;
        if n > MAX_ITEMS {
            return Err(DecodeError("候选太多"));
        }
        let mut items = Vec::with_capacity(n);
        let mut corrected = Vec::with_capacity(n);
        for _ in 0..n {
            items.push(self.str()?);
            corrected.push(self.bool()?);
        }
        let highlight = self.u32()? as usize;
        let page = self.u32()? as usize;
        let pages = self.u32()? as usize;
        let total = self.u32()? as usize;
        let english_mode = self.bool()?;
        let armed = self.bool()?;
        if n > 0 && highlight >= n {
            return Err(DecodeError("高亮位置不对"));
        }
        Ok(View { composing, preedit, mode, items, highlight, page, pages, total, corrected, english_mode, armed })
    }
    fn done(&self) -> Result<(), DecodeError> {
        if self.p == self.b.len() {
            Ok(())
        } else {
            Err(DecodeError("消息后面有多余的内容"))
        }
    }
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc(Vec::new());
        match self {
            Request::Hello { proto, pid } => {
                e.u8(1);
                e.u32(*proto);
                e.u32(*pid);
            }
            Request::Key { key, mods } => {
                e.u8(2);
                e.key(*key);
                e.mods(*mods);
            }
            Request::TestKey { key, mods } => {
                e.u8(3);
                e.key(*key);
                e.mods(*mods);
            }
            Request::Reset => e.u8(4),
            Request::ClearContext => e.u8(5),
            Request::GetState => e.u8(6),
            Request::ReloadSettings => e.u8(7),
            Request::SetEnglishMode { on } => {
                e.u8(8);
                e.bool(*on);
            }
            Request::AddWords { text } => {
                e.u8(9);
                e.str(text);
            }
            Request::SetPrivate { on } => {
                e.u8(10);
                e.bool(*on);
            }
        }
        e.0
    }

    pub fn decode(b: &[u8]) -> Result<Request, DecodeError> {
        let mut d = Dec { b, p: 0 };
        let r = match d.u8()? {
            1 => Request::Hello { proto: d.u32()?, pid: d.u32()? },
            2 => Request::Key { key: d.key()?, mods: d.mods()? },
            3 => Request::TestKey { key: d.key()?, mods: d.mods()? },
            4 => Request::Reset,
            5 => Request::ClearContext,
            6 => Request::GetState,
            7 => Request::ReloadSettings,
            8 => Request::SetEnglishMode { on: d.bool()? },
            9 => Request::AddWords { text: d.str()? },
            10 => Request::SetPrivate { on: d.bool()? },
            _ => return Err(DecodeError("请求类型不对")),
        };
        d.done()?;
        Ok(r)
    }
}

impl Reply {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc(Vec::new());
        match self {
            Reply::Hello { proto } => {
                e.u8(1);
                e.u32(*proto);
            }
            Reply::KeyResult { consumed, commit, view } => {
                e.u8(2);
                e.bool(*consumed);
                match commit {
                    Some(c) => {
                        e.bool(true);
                        e.str(c);
                    }
                    None => e.bool(false),
                }
                e.view(view);
            }
            Reply::Bool(v) => {
                e.u8(3);
                e.bool(*v);
            }
            Reply::Ok => e.u8(4),
            Reply::State { english_mode, chinese_punct } => {
                e.u8(5);
                e.bool(*english_mode);
                e.bool(*chinese_punct);
            }
            Reply::WordsAdded { added, skipped } => {
                e.u8(6);
                e.u32(*added);
                let n = skipped.len().min(MAX_ITEMS);
                e.u32(n as u32);
                for x in &skipped[..n] {
                    e.str(x);
                }
            }
            Reply::Error(s) => {
                e.u8(255);
                e.str(s);
            }
        }
        e.0
    }

    pub fn decode(b: &[u8]) -> Result<Reply, DecodeError> {
        let mut d = Dec { b, p: 0 };
        let r = match d.u8()? {
            1 => Reply::Hello { proto: d.u32()? },
            2 => {
                let consumed = d.bool()?;
                let commit = if d.bool()? { Some(d.str()?) } else { None };
                Reply::KeyResult { consumed, commit, view: d.view()? }
            }
            3 => Reply::Bool(d.bool()?),
            4 => Reply::Ok,
            5 => Reply::State { english_mode: d.bool()?, chinese_punct: d.bool()? },
            6 => {
                let added = d.u32()?;
                let n = d.u32()? as usize;
                if n > MAX_ITEMS {
                    return Err(DecodeError("跳过的词太多"));
                }
                let mut skipped = Vec::with_capacity(n);
                for _ in 0..n {
                    skipped.push(d.str()?);
                }
                Reply::WordsAdded { added, skipped }
            }
            255 => Reply::Error(d.str()?),
            _ => return Err(DecodeError("回复类型不对")),
        };
        d.done()?;
        Ok(r)
    }
}

// ---------------- 传输 ----------------

pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "消息太长"));
    }
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "消息太长"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// 发一个请求、等一个回复。
pub fn call<S: Read + Write>(s: &mut S, req: &Request) -> io::Result<Reply> {
    write_frame(s, &req.encode())?;
    let b = read_frame(s)?;
    Reply::decode(&b).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

// ---------------- 设置 ----------------

/// 你的记忆和设置放在哪：%APPDATA%\PinyinIME（只有你能读写）。
/// 环境变量 PINYIN_USER_DIR 可以改（测试用）。Mac / Linux 上是 ~/.pinyin-ime-engine/PinyinIME。
pub fn user_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PINYIN_USER_DIR") {
        return PathBuf::from(p);
    }
    #[cfg(windows)]
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    #[cfg(not(windows))]
    let base = std::env::var("HOME").map(|h| format!("{h}/.pinyin-ime-engine")).unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("PinyinIME")
}

/// 设置页在引擎没运行时，要加入的词先写到这个文件（和 settings.ini 在同一个文件夹），引擎启动时读进来。
pub const PENDING_WORDS: &str = "pending_words.txt";

pub fn settings_path() -> PathBuf {
    user_dir().join("settings.ini")
}

/// 设置（存在 %APPDATA%\PinyinIME\settings.ini，一行一项：键=值）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// 中文标点
    pub chinese_punct: bool,
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        let mut s = Settings::default();
        let Ok(text) = std::fs::read_to_string(path) else { return s };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let on = matches!(v.trim(), "1" | "true" | "yes");
            // 以前的 style=（日常/书面）不用了，读到就忽略
            if k.trim() == "chinese_punct" {
                s.chinese_punct = on;
            }
        }
        s
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = format!(
            "# 拼音输入法设置（也可以在桌面上的\"拼音输入法设置\"里改）\nchinese_punct={}\n",
            if self.chinese_punct { "true" } else { "false" }
        );
        // 先写临时文件再改名，写到一半断电也不会坏
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Hello { proto: PROTO, pid: 1234 },
            Request::Key { key: Key::Char('你'), mods: Mods { ctrl: true, alt: false, caps_lock: true } },
            Request::Key { key: Key::ShiftTap, mods: Mods::default() },
            Request::TestKey { key: Key::Down, mods: Mods::default() },
            Request::Reset,
            Request::ClearContext,
            Request::GetState,
            Request::ReloadSettings,
            Request::SetEnglishMode { on: true },
            Request::AddWords { text: "张子涵，螺蛳粉 曾经(zeng jing)".into() },
            Request::SetPrivate { on: true },
            Request::SetPrivate { on: false },
        ]
    }

    #[test]
    fn requests_roundtrip() {
        for r in all_requests() {
            assert_eq!(Request::decode(&r.encode()).unwrap(), r);
        }
    }

    #[test]
    fn replies_roundtrip() {
        let view = View {
            composing: true,
            preedit: "你hao".into(),
            mode: Mode::Grid,
            items: vec!["好".into(), "号".into()],
            highlight: 1,
            page: 0,
            pages: 1,
            total: 2,
            corrected: vec![false, true],
            english_mode: false,
            armed: true,
        };
        for r in [
            Reply::Hello { proto: 1 },
            Reply::KeyResult { consumed: true, commit: Some("你好".into()), view: view.clone() },
            Reply::KeyResult { consumed: false, commit: None, view },
            Reply::Bool(true),
            Reply::Ok,
            Reply::State { english_mode: true, chinese_punct: false },
            Reply::WordsAdded { added: 3, skipped: vec!["abc（有字母）".into()] },
            Reply::Error("出错".into()),
        ] {
            assert_eq!(Reply::decode(&r.encode()).unwrap(), r);
        }
    }

    #[test]
    fn garbage_is_rejected_not_crashing() {
        // 随便改坏消息：要么解出来，要么报错，不能崩溃
        let mut x: u64 = 99;
        for r in all_requests() {
            let good = r.encode();
            for i in 0..good.len() {
                for _ in 0..8 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let mut v = good.clone();
                    v[i] = x as u8;
                    let _ = Request::decode(&v);
                    let _ = Reply::decode(&v);
                }
                let _ = Request::decode(&good[..i]);
            }
        }
        assert!(Request::decode(&[]).is_err());
        assert!(Request::decode(&[4, 0]).is_err());
    }

    #[test]
    fn settings_file() {
        let dir = std::env::temp_dir().join(format!("pyime-set-{}", std::process::id()));
        let p = dir.join("settings.ini");
        assert_eq!(Settings::load(&p), Settings::default());
        let s = Settings { chinese_punct: true };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
