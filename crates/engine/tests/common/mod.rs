use std::path::PathBuf;

use pinyin_engine::Dict;

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
