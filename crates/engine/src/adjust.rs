//! 个别词的分数调整：名单（adjust.tsv）加一条地理说法的规则。
//!
//! - 正常的粗口排在网上避讳的写法前面（妈的 在 码的、尼玛 前面）
//! - 很老的谐音梗往后排（杯具、神马、鸭梨）
//! - 地理描述往后排："东西南北 + 街/门/站/郊……"（西街、北站），"东西南北 + 接/临/邻/至……"（西接、东临）。
//!   这类说法在百科里很多，平时聊天很少用，以前常常排在"细节"这种常用词旁边

use std::collections::HashMap;
use std::sync::OnceLock;

/// 地理说法往后排这么多
const GEO_PEN: f32 = -2.5;
const DIRS: &str = "东西南北";
/// 地名后缀（西街、北站、东郊）
const PLACE_SUFFIX: &str = "街门站郊区城关港岸巷坊";
/// 地理描述（西接、东临、北至）
const GEO_VERB: &str = "接临邻至抵连靠濒";

fn table() -> &'static HashMap<String, f32> {
    static T: OnceLock<HashMap<String, f32>> = OnceLock::new();
    T.get_or_init(|| {
        include_str!("adjust.tsv")
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let (w, v) = l.split_once('\t')?;
                Some((w.trim().to_string(), v.trim().parse().ok()?))
            })
            .collect()
    })
}

/// 这个词的分数调整（大多数词是 0）。
pub fn adjust(text: &str) -> f32 {
    if let Some(&v) = table().get(text) {
        return v;
    }
    let mut cs = text.chars();
    if let (Some(a), Some(b), None) = (cs.next(), cs.next(), cs.next()) {
        if DIRS.contains(a) && (PLACE_SUFFIX.contains(b) || GEO_VERB.contains(b)) {
            return GEO_PEN;
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_and_rule() {
        assert!(adjust("妈的") > 0.0);
        assert!(adjust("尼玛") < 0.0);
        assert!(adjust("杯具") < 0.0);
        assert!(adjust("西街") < 0.0);
        assert!(adjust("西接") < 0.0);
        assert!(adjust("北站") < 0.0);
        // 常用词不受影响
        for w in ["东西", "南北", "北京", "西安", "细节", "东边", "西方", "南方", "权重"] {
            assert_eq!(adjust(w), 0.0, "{w}");
        }
        // 名单里每一行都读进来了
        assert!(table().len() >= 50);
    }
}
