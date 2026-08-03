//! dss-skills: SKILL.md 解析 + 多源加载 + BM25+Jaccard/RRF 检索。
//!
//! P5a：builtin + global + project 源。paper-writing 编排链留 P5b。

pub mod bm25;
pub mod catalog;
pub mod frontmatter;
pub mod skill;

pub use catalog::{global_skills_dir, project_skills_dir, render_skill_body, seed_builtin_to_global, SkillCatalog};
pub use skill::{Skill, SkillHit};
