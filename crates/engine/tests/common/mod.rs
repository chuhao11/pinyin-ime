use std::path::PathBuf;

use pinyin_engine::{Dict, EnglishDict, Ime, UserDb};

/// 打开真实词库。没有编译过词库时：设置了 REQUIRE_DICT（CI 里）就报错，否则跳过。
pub fn real_dict() -> Option<Dict> {
    let p = std::env::var("PINYIN_DICT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/build/pinyin.dict"));
    if !p.exists() {
        if std::env::var("REQUIRE_DICT").is_ok() {
            panic!("找不到词库 {}，请先运行 cargo run --release -p dictgen", p.display());
        }
        eprintln!("跳过：找不到词库 {}（先运行 cargo run --release -p dictgen）", p.display());
        return None;
    }
    Some(Dict::open(&p).expect("词库打不开"))
}

/// 英文词表（和词库在同一个文件夹）。
#[allow(dead_code)]
pub fn real_english() -> Option<EnglishDict> {
    let p = std::env::var("PINYIN_DICT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/build/pinyin.dict"));
    let en = EnglishDict::load(&p.with_file_name("english.tsv")).ok();
    if en.is_none() && std::env::var("REQUIRE_DICT").is_ok() {
        panic!("找不到英文词表 english.tsv");
    }
    en
}

/// 装好词库和英文词表的输入法。
#[allow(dead_code)]
pub fn real_ime(user: UserDb) -> Option<Ime> {
    let dict = real_dict()?;
    let mut ime = Ime::new(dict, user);
    if let Some(en) = real_english() {
        ime.set_english(en);
    }
    if std::env::var("PINYIN_NO_LM").is_err() {
        let p = std::env::var("PINYIN_DICT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/build/pinyin.dict"));
        if let Ok(lm) = pinyin_engine::Lm::open(&p.with_file_name("pinyin.lm")) {
            ime.set_lm(lm);
        }
    }
    ime.set_clock(|| 1_800_000_000);
    Some(ime)
}
