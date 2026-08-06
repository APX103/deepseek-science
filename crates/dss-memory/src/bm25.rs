//! memory recall BM25：k1=1.5, b=0.75，Okapi IDF，**CJK 每字成 token**。
//!
//! 注意：memory 的 k1=1.5 与 skills 的 k1=1.2 是两套独立常量（modules.md 决策，不要统一）。
//!
//! 两套召回路径：
//! - `recall()`：无状态，每次对候选集全量分词打分。适合一次性/小集合（consolidate 去重）。
//! - `RecallIndex`：持久化倒排索引，构建一次后多次查询 O(候选token数)。
//!   MemoryStore 懒加载缓存 + 写入失效策略（本地应用记忆量小，增量更新是过度工程）。

use std::collections::{HashMap, HashSet};

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

    let mut df: HashMap<String, f64> = HashMap::new();
    for d in &docs {
        let uniq: HashSet<&String> = d.iter().collect();
        for t in uniq {
            *df.entry(t.clone()).or_insert(0.0) += 1.0;
        }
    }

    let q_terms = tokenize(query);
    let mut scored: Vec<(&MemoryRow, f64)> = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        let s = score_doc(d, &q_terms, &df, n, avgdl);
        if s > 0.0 {
            scored.push((&candidates[i], s));
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

/// 单文档 BM25 打分（提取出来复用于 self_score）。
fn score_doc(
    doc: &[String],
    q_terms: &[String],
    df: &HashMap<String, f64>,
    n: f64,
    avgdl: f64,
) -> f64 {
    let dl = doc.len() as f64;
    let mut tf: HashMap<String, f64> = HashMap::new();
    for t in doc {
        *tf.entry(t.clone()).or_insert(0.0) += 1.0;
    }
    let mut s = 0.0f64;
    for t in q_terms {
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
    s
}

/// 一个文档对自身查询的 BM25 分数，用作近似重复判定的归一化上界。
///
/// 语义：把文档的 token 当作 query，在「仅含该文档的语料」里算它的分数。
/// consolidate 用 score(query=新记忆, doc=旧记忆) / self_score(旧记忆) 作相似度代理。
pub fn self_score(_doc: &MemoryRow, body_used_as_query: &str) -> f64 {
    // 用 body 的 token 作为 query；语料只含这一个文档，所以 df 恒为 1，n=1。
    let doc = tokenize(body_used_as_query);
    if doc.is_empty() {
        return 0.0;
    }
    let n = 1.0f64;
    let avgdl = doc.len() as f64;
    let mut df: HashMap<String, f64> = HashMap::new();
    for t in &doc {
        *df.entry(t.clone()).or_insert(0.0) += 1.0;
    }
    // 复用 score_doc：q_terms = doc 的去重 token 序列
    let q_terms: Vec<String> = doc.to_vec();
    score_doc(&doc, &q_terms, &df, n, avgdl)
}

// ----------------- 持久化倒排索引 -----------------

/// 预构建的 BM25 倒排索引：构建一次后多次查询，避免每次 recall 全量重分词。
///
/// 策略：MemoryStore 懒加载 + 写入失效。对本地应用（记忆量小、写入少），
/// 比写入时增量维护倒排表更简单可靠。
pub struct RecallIndex {
    /// doc id → 分词后的 token 序列（保留重复，用于 tf 计算）。
    docs: HashMap<String, Vec<String>>,
    /// token → 包含该 token 的 doc id 集合（df 来源）。
    df: HashMap<String, HashSet<String>>,
    /// doc id → 文档长度（token 数）。
    doc_len: HashMap<String, usize>,
    n: usize,
    avgdl: f64,
}

impl RecallIndex {
    /// 从候选记忆集合构建索引（一次性全量分词）。
    pub fn build(candidates: &[MemoryRow]) -> Self {
        let mut docs = HashMap::new();
        let mut df: HashMap<String, HashSet<String>> = HashMap::new();
        let mut doc_len = HashMap::new();
        let mut total_len = 0usize;
        for m in candidates {
            let toks = tokenize(&m.body);
            total_len += toks.len();
            let uniq: HashSet<&String> = toks.iter().collect();
            for t in uniq {
                df.entry(t.clone()).or_default().insert(m.id.clone());
            }
            doc_len.insert(m.id.clone(), toks.len());
            docs.insert(m.id.clone(), toks);
        }
        let n = candidates.len();
        let avgdl = if n == 0 {
            0.0
        } else {
            total_len as f64 / n as f64
        };
        Self {
            docs,
            df,
            doc_len,
            n,
            avgdl,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// 用索引对 query 打分，返回 (doc_id, score) 按 score 降序，只含 score > 0。
    pub fn search(&self, query: &str, top_n: usize) -> Vec<(String, f64)> {
        if self.is_empty() {
            return Vec::new();
        }
        let q_terms = tokenize(query);
        let n = self.n as f64;
        let mut scored: Vec<(String, f64)> = Vec::new();
        // 遍历所有文档打分（文档级并行无意义，量小）。
        for (id, doc) in &self.docs {
            let dl = *self.doc_len.get(id).unwrap_or(&0) as f64;
            let mut tf: HashMap<&str, f64> = HashMap::new();
            for t in doc {
                *tf.entry(t.as_str()).or_insert(0.0) += 1.0;
            }
            let mut s = 0.0f64;
            for t in &q_terms {
                let f = match tf.get(t.as_str()) {
                    Some(v) => *v,
                    None => continue,
                };
                let df_t = self.df.get(t).map(|s| s.len() as f64).unwrap_or(0.0);
                if df_t == 0.0 {
                    continue;
                }
                let idf = (((n - df_t + 0.5) / (df_t + 0.5)) + 1.0).ln().max(0.0);
                let denom = f + K1 * (1.0 - B + B * (dl / self.avgdl.max(1.0)));
                s += idf * (f * (K1 + 1.0)) / denom;
            }
            if s > 0.0 {
                scored.push((id.clone(), s));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_n);
        scored
    }
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
            status: "active".into(),
            claim_type: "note".into(),
            evidence_refs: None,
            origin: "auto".into(),
            superseded_by: None,
            valid_from: None,
            valid_until: None,
            deleted_at: None,
            source_hash: None,
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

    #[test]
    fn self_score_positive_for_real_text() {
        let m = mk("1", "用户使用 Rust 编程语言", None);
        assert!(self_score(&m, "用户使用 Rust 编程语言") > 0.0);
    }

    #[test]
    fn self_score_zero_for_empty() {
        let m = mk("1", "的 了 是", None); // 全停用词 → 空 token
        assert_eq!(self_score(&m, "的 了 是"), 0.0);
    }

    #[test]
    fn recall_index_ranks_relevant_first() {
        let cands = vec![
            mk("1", "用户研究钙钛矿太阳电池", None),
            mk("2", "今晚吃火锅", None),
        ];
        let idx = RecallIndex::build(&cands);
        let hits = idx.search("钙钛矿 太阳电池", 5);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0, "1");
        assert!(hits[0].1 > 0.0);
    }

    #[test]
    fn recall_index_respects_top_n() {
        let cands = vec![
            mk("1", "钙钛矿电池效率", None),
            mk("2", "钙钛矿稳定性", None),
            mk("3", "钙钛矿毒性", None),
        ];
        let idx = RecallIndex::build(&cands);
        let hits = idx.search("钙钛矿", 2);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn recall_index_empty_for_no_match() {
        let cands = vec![mk("1", "some memory", None)];
        let idx = RecallIndex::build(&cands);
        assert!(idx.search("zzznomatch", 5).is_empty());
    }
}
