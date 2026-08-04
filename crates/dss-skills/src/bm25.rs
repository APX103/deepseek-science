//! BM25(k1=1.2, b=0.75) + Jaccard，RRF(k=60, threshold=0.029) 融合（modules.md §5）。
//!
//! 注意：skills 的 BM25 k1=1.2，与 memory 的 k1=1.5 是两套独立常量（modules.md 决策）。
//! CJK 不做每字成 token（与 memory 不同——skills 文本多为英文描述；保持简单）。

use crate::skill::{Skill, SkillHit};

const K1: f64 = 1.2;
const B: f64 = 0.75;
const RRF_K: f64 = 60.0;
const RRF_THRESHOLD: f64 = 0.029;

/// 分词：小写 + 按非字母数字分割。
pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// BM25 排序：返回 (skill_index, score) 列表（按分数降序）。
pub fn bm25_ranks(skills: &[Skill], query: &str) -> Vec<(usize, f64)> {
    if skills.is_empty() {
        return Vec::new();
    }
    let docs: Vec<Vec<String>> = skills
        .iter()
        .map(|s| tokenize(&format!("{} {}", s.name, s.description)))
        .collect();
    let n = docs.len() as f64;
    let avgdl: f64 = docs.iter().map(|d| d.len() as f64).sum::<f64>() / n.max(1.0);

    // df（含某 term 的文档数）。
    let mut df: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for d in &docs {
        let uniq: std::collections::HashSet<&String> = d.iter().collect();
        for t in uniq {
            *df.entry(t.clone()).or_insert(0.0) += 1.0;
        }
    }

    let q_terms = tokenize(query);
    let mut scores = vec![0.0f64; skills.len()];
    for (i, d) in docs.iter().enumerate() {
        let dl = d.len() as f64;
        // tf
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
            // Okapi IDF（非负下限）。
            let idf = (((n - df_t + 0.5) / (df_t + 0.5)) + 1.0).ln().max(0.0);
            let denom = f + K1 * (1.0 - B + B * (dl / avgdl.max(1.0)));
            s += idf * (f * (K1 + 1.0)) / denom;
        }
        scores[i] = s;
    }

    let mut ranked: Vec<(usize, f64)> = scores
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, s)| *s > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
}

/// Jaccard 排序（query tokens vs skill name+desc tokens 的集合相似度）。
pub fn jaccard_ranks(skills: &[Skill], query: &str) -> Vec<(usize, f64)> {
    let q: std::collections::HashSet<String> = tokenize(query).into_iter().collect();
    if q.is_empty() {
        return Vec::new();
    }
    let mut ranked = Vec::new();
    for (i, s) in skills.iter().enumerate() {
        let d: std::collections::HashSet<String> =
            tokenize(&format!("{} {}", s.name, s.description))
                .into_iter()
                .collect();
        let inter = q.intersection(&d).count() as f64;
        let union = q.union(&d).count() as f64;
        if union == 0.0 {
            continue;
        }
        let j = inter / union;
        if j > 0.0 {
            ranked.push((i, j));
        }
    }
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
}

/// RRF 融合 BM25 + Jaccard，返回按融合分数降序的命中（过滤 threshold）。
pub fn search(skills: &[Skill], query: &str) -> Vec<SkillHit> {
    let bm = bm25_ranks(skills, query);
    let jac = jaccard_ranks(skills, query);
    // RRF：score = Σ 1/(k + rank)，rank 从 1 起。
    let mut rrf: std::collections::HashMap<usize, f64> = std::collections::HashMap::new();
    for (rank, (idx, _)) in bm.iter().enumerate() {
        *rrf.entry(*idx).or_insert(0.0) += 1.0 / (RRF_K + (rank as f64 + 1.0));
    }
    for (rank, (idx, _)) in jac.iter().enumerate() {
        *rrf.entry(*idx).or_insert(0.0) += 1.0 / (RRF_K + (rank as f64 + 1.0));
    }
    let mut hits: Vec<(usize, f64)> = rrf.into_iter().collect();
    hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    hits.into_iter()
        .filter(|(_, s)| *s >= RRF_THRESHOLD)
        .map(|(i, s)| SkillHit {
            name: skills[i].name.clone(),
            description: skills[i].description.clone(),
            source: skills[i].source.clone(),
            score: s,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(name: &str, desc: &str) -> Skill {
        Skill {
            name: name.into(),
            description: desc.into(),
            source: "test".into(),
            body: String::new(),
        }
    }

    #[test]
    fn search_returns_relevant_skill_first() {
        let skills = vec![
            mk("paper-writing", "Write an academic survey paper with latex"),
            mk("lit-survey", "Survey literature on a topic"),
            mk("cooking", "Recipes and meal planning"),
        ];
        let hits = search(&skills, "write a research paper survey");
        assert!(!hits.is_empty());
        // paper-writing / lit-survey 应排在 cooking 前。
        let top_names: Vec<&str> = hits.iter().take(2).map(|h| h.name.as_str()).collect();
        assert!(top_names.contains(&"paper-writing") || top_names.contains(&"lit-survey"));
        assert!(!top_names.contains(&"cooking"));
    }

    #[test]
    fn search_no_match_returns_empty() {
        let skills = vec![mk("paper-writing", "academic writing")];
        assert!(search(&skills, "zzznoexist").is_empty());
    }
}
