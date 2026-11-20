//! 拼音输入法引擎核心（和平台无关）。
//!
//! - `dict`：只读词库（内存映射）
//! - `pinyin`：拼音切分
//! - `english`：英文单词（原样、补全、拼写纠错）
//! - `lattice`：词网和候选生成
//! - `userdb`：用户词库（记忆）
//! - `session`：按键规则

pub mod dict;
pub mod english;
pub mod lattice;
pub mod pinyin;
pub mod session;
pub mod userdb;

pub use dict::{Dict, DictBuilder, DictError};
pub use english::EnglishDict;
pub use lattice::{CandKind, Candidate};
pub use session::{display_width, Config, Ime, Key, Mode, Mods, Response, View, GRID_COLS, GRID_SIZE};
pub use userdb::UserDb;
