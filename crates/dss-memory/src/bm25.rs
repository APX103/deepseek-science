//! memory recall BM25：k1=1.5, b=0.75，Okapi IDF，**CJK 每字成 token**。
//!
//! 注意：memory 的 k1=1.5 与 skills 的 k1=1.2 是两套独立常量（modules.md 决策，不要统一）。

use dss_db::repo::MemoryRow;

const K1: f64 = 1.5;
const B: f64 = 0.75;

/// 英文停用词（精简）。
const STOPWORDS_EN: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "of", "to", "in", "on", "at", "for", "is", "are", "was",
    "were", "be", "been", "being", "this", "that", "these", "those", "it", "its", "as", "by",
    "with", "from", "about", "into", "i", "you", "he", "she", "we", "they",
];

/// 中文停用词（精简）。
const STOPWORDS_ZH: &[&str] = &[
    "的", "了", "是", "在", "我", "你", "他", "她", "它", "们", "和", "与", "或", "也", "都", "这",
    "那", "有", "为", "以", "及", "等",
];

/// 分词：CJK 每字成 token；英文按非字母数字分割；去停用词。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let push_buf = |buf: &mut String, out: &mut Vec<String>| {
        if !buf.is_empty() {
            let w = buf.to_lowercase();
            if !STOPWORDS_EN.contains(&w.as_str()) {
                out.push(w);
            }
            buf.clear();
        }
    };
    for ch in text.chars() {
        if ch.is_alphanumeric() && !is_cjk(ch) {
            buf.push(ch);
        } else {
            push_buf(&mut buf, &mut out);
            if is_cjk(ch) {
                let s = ch.to_string();
                if !STOPWORDS_ZH.contains(&s.as_str()) {
                    out.push(s);
                }
            }
        }
    }
    push_buf(&mut buf, &mut out);
    out
}

fn is_cjk(ch: char) -> bool {
    let c = ch as u32;
    // CJK 统一表意 + 扩展 A + 常用中日韩。
    (0x4E00..=0x9FFF).contains(&c)
        || (0x3400..=0x4DBF).contains(&c)
        || (0x3000..=0x303F).contains(&c)
}

/// BM25 召回：从候选记忆里按 query 排序，返回带分数的列表（分数 > 0）。
pub fn recall<'a>(candidates: &'a [MemoryRow], query: &str) -> Vec<(&'a MemoryRow, f64)> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let docs: Vec<Vec<String>> = candidates.iter().map(|m| tokenize(&m.body)).collect();
    let n = docs.len() as f64;
    let avgdl: f64 = docs.iter().map(|d| d.len() as f64).sum::<f64>() / n.max(1.0);

    let mut df: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for d in &docs {
        let uniq: std::collections::HashSet<&String> = d.iter().collect();
        for t in uniq {
            *df.entry(t.clone()).or_insert(0.0) += 1.0;
        }
    }

    let q_terms = tokenize(query);
    let mut scored: Vec<(&MemoryRow, f64)> = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        let dl = d.len() as f64;
        let mut tf: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for t in d {
            *tf.entry(t.clone()).or_insert(0.0) += 1.0;
        }
        let mut s = 0.0f64;
        for t in &q_terms {
            let f = match tf.get(t) {
                Some(v) => *v,
                None => continue,
            };
            let df_t = *df.get(t).unwrap_or(&0.0);
            if df_t == 0.0 {
                continue;
            }
            let idf = (((n - df_t + 0.5) / (df_t + 0.5)) + 1.0).ln().max(0.0);
            let denom = f + K1 * (1.0 - B + B * (dl / avgdl.max(1.0)));
            s += idf * (f * (K1 + 1.0)) / denom;
        }
        if s > 0.0 {
            scored.push((&candidates[i], s));
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(id: &str, body: &str, project: Option<&str>) -> MemoryRow {
        MemoryRow {
            id: id.into(),
            entity: "project".into(),
            scope: Some("project".into()),
            entity_type: "note".into(),
            body: body.into(),
            project_id: project.map(|p| p.into()),
            confidence: 0.5,
            created_at: "t".into(),
            updated_at: "t".into(),
            last_surfaced_at: None,
        }
    }

    #[test]
    fn cjk_each_char_is_token() {
        let toks = tokenize("钙钛矿太阳电池");
        // 每个 CJK 字符一个 token（7 字）。
        assert_eq!(toks.len(), 7);
        assert!(toks.contains(&"钙".to_string()));
    }

    #[test]
    fn recall_ranks_relevant_first() {
        let cands = vec![
            mk("1", "用户研究钙钛矿太阳电池，偏好无铅材料", Some("proj_a")),
            mk("2", "今晚吃火锅", Some("proj_a")),
        ];
        let hits = recall(&cands, "钙钛矿 太阳电池");
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0.id, "1");
    }

    #[test]
    fn recall_no_match_empty() {
        let cands = vec![mk("1", "some memory", Some("p"))];
        assert!(recall(&cands, "zzznomatch").is_empty());
    }
}
