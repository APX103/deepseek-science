//! Skill 类型。

/// 一个已加载的 skill。
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// 来源：builtin / global / claude / project / custom。
    pub source: String,
    /// markdown body（frontmatter 之后的正文）。
    pub body: String,
}

/// 检索命中。
#[derive(Debug, Clone)]
pub struct SkillHit {
    pub name: String,
    pub description: String,
    pub source: String,
    pub score: f64,
}
