//! Claim Store 领域类型：记忆分类、生命周期状态、来源、证据引用。
//!
//! 这些枚举的字符串值与数据库列值（schema.rs 的 DEFAULT）及 extract prompt 约定一致。
//! 改动任一处需同步：schema DEFAULT / extract EXTRACT_SYSTEM / 这里。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 记忆类型：决定抽取方式、召回权重、保留周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaimType {
    /// 稳定事实（用户身份、技术栈、环境）
    Fact,
    /// 偏好/习惯（高风险，默认需审批）
    Preference,
    /// 决策（高风险，默认需审批）
    Decision,
    /// 可复用步骤/工具用法
    Procedure,
    /// 仓库相关（架构、约定、调试经验）
    Repo,
    /// 未分类笔记
    #[default]
    Note,
}

impl ClaimType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClaimType::Fact => "fact",
            ClaimType::Preference => "preference",
            ClaimType::Decision => "decision",
            ClaimType::Procedure => "procedure",
            ClaimType::Repo => "repo",
            ClaimType::Note => "note",
        }
    }

    /// 是否高风险（涉及偏好/决策）→ 默认进 candidate 队列等审批。
    pub fn is_high_risk(&self) -> bool {
        matches!(self, ClaimType::Preference | ClaimType::Decision)
    }

    /// 从字符串解析（LLM 输出 / 数据库列值）。未识别 → Note（fail-safe）。
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "fact" => ClaimType::Fact,
            "preference" => ClaimType::Preference,
            "decision" => ClaimType::Decision,
            "procedure" => ClaimType::Procedure,
            "repo" => ClaimType::Repo,
            _ => ClaimType::Note,
        }
    }
}

/// 记忆生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStatus {
    /// 生效中，可召回。
    Active,
    /// 待人工审批（高风险抽取默认进此态）。
    Candidate,
    /// 被新 claim 替代（superseded_by 指向新 id）。
    Superseded,
    /// 已过期（valid_until 超期）。
    Expired,
    /// 软删除（deleted_at 已置，保留审计）。
    Deleted,
}

impl MemoryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryStatus::Active => "active",
            MemoryStatus::Candidate => "candidate",
            MemoryStatus::Superseded => "superseded",
            MemoryStatus::Expired => "expired",
            MemoryStatus::Deleted => "deleted",
        }
    }

    /// 是否可参与召回（active 才会被召回）。
    pub fn recallable(&self) -> bool {
        matches!(self, MemoryStatus::Active)
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(MemoryStatus::Active),
            "candidate" => Some(MemoryStatus::Candidate),
            "superseded" => Some(MemoryStatus::Superseded),
            "expired" => Some(MemoryStatus::Expired),
            "deleted" => Some(MemoryStatus::Deleted),
            _ => None,
        }
    }
}

/// 记忆来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// 后台 LLM 自动抽取。
    Auto,
    /// 用户显式 remember（HTTP POST）。
    Explicit,
    /// 外部导入。
    Imported,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::Auto => "auto",
            Origin::Explicit => "explicit",
            Origin::Imported => "imported",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "explicit" => Origin::Explicit,
            "imported" => Origin::Imported,
            _ => Origin::Auto,
        }
    }
}

/// 证据引用：一条 claim 可指向 L1（session_messages）的多个范围。
/// 存储为 evidence_refs 列的 JSON 数组。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub session_id: String,
    pub run_id: Option<String>,
    pub seq_start: i64,
    pub seq_end: i64,
}

/// 归一化记忆文本并取 sha256 前 16 hex 作为去重指纹。
///
/// 归一化：转小写、CR/换行/制表→空格、压缩连续空格、去首尾空白。
/// CJK 字符不变（不需要分词，只做字节级去重）。
pub fn memory_hash(body: &str) -> String {
    let mut normalized = String::with_capacity(body.len());
    let mut prev_space = false;
    for c in body.chars() {
        if c == '\n' || c == '\r' || c == '\t' || c == ' ' {
            if !prev_space {
                normalized.push(' ');
            }
            prev_space = true;
        } else {
            normalized.push(c.to_ascii_lowercase());
            prev_space = false;
        }
    }
    let normalized = normalized.trim();
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    digest
        .iter()
        .take(8)
        .map(|b| format!("{:02x}", b))
        .collect()
}

/// 生成记忆/事件 id：`mem_<12hex>` / `mev_<12hex>`。
pub fn gen_id(prefix: &str) -> String {
    format!(
        "{}{}",
        prefix,
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_normalizes_whitespace_and_case() {
        let a = memory_hash("用户  喜欢\nRust");
        let b = memory_hash("用户 喜欢 RUST");
        let c = memory_hash("  用户\t喜欢 Rust  ");
        assert_eq!(a, b, "换行/大小写应归一");
        assert_eq!(a, c, "首尾空白和制表符应归一");
    }

    #[test]
    fn hash_is_16_hex_chars() {
        let h = memory_hash("hello");
        assert_eq!(h.len(), 16, "应为 8 字节 = 16 hex 字符");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_distinguishes_different_bodies() {
        assert_ne!(memory_hash("用 Rust"), memory_hash("用 Python"));
    }

    #[test]
    fn claim_type_high_risk_only_preference_decision() {
        assert!(ClaimType::Preference.is_high_risk());
        assert!(ClaimType::Decision.is_high_risk());
        assert!(!ClaimType::Fact.is_high_risk());
        assert!(!ClaimType::Procedure.is_high_risk());
        assert!(!ClaimType::Repo.is_high_risk());
        assert!(!ClaimType::Note.is_high_risk());
    }

    #[test]
    fn claim_type_parse_fail_safe_to_note() {
        assert_eq!(ClaimType::parse("garbage"), ClaimType::Note);
        assert_eq!(ClaimType::parse("FACT"), ClaimType::Fact);
        assert_eq!(ClaimType::parse(" Decision "), ClaimType::Decision);
    }

    #[test]
    fn status_recallable() {
        assert!(MemoryStatus::Active.recallable());
        assert!(!MemoryStatus::Candidate.recallable());
        assert!(!MemoryStatus::Deleted.recallable());
    }
}
