//! 拼音输入法引擎核心（和平台无关）。
//!
//! - `dict`：只读词库（内存映射）
//! - `pinyin`：拼音切分
//! - `english`：英文单词（原样、补全、拼写纠错）
//! - `lm`：语言模型（词和词的搭配）
//! - `lattice`：词网和候选生成
//! - `userdb`：用户词库（记忆）
//! - `session`：按键规则
//! - `service`：引擎进程收到请求后怎么处理

pub mod addwords;
pub mod adjust;
pub mod dict;
pub mod english;
pub mod lattice;
pub mod lm;
pub mod pinyin;
pub mod service;
pub mod session;
pub mod userdb;

pub use dict::{Dict, DictBuilder, DictError};
pub use english::EnglishDict;
pub use lattice::{analyze_with, CandKind, Candidate, Query};
pub use lm::Lm;
pub use service::Service;
pub use session::{
    display_width, AddResult, Config, Engine, Ime, Key, Mode, Mods, Response, Session, View, GRID_COLS, GRID_SIZE,
};
pub use userdb::UserDb;
