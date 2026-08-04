//! Skill 目录 + 多源加载。
//!
//! P5a 源：builtin（include_dir! 嵌入）→ global(data_dir/skills) → project(workspace/.dss/skills)。
//! 首跑复制 builtin 到 global（不覆盖）。claude/custom 源留 P5b。
//! 后源覆盖前源（同名 skill 取后加载的）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::bm25;
use crate::frontmatter::parse_skill;
use crate::skill::{Skill, SkillHit};

/// 嵌入的内置 skills（编译期打进二进制）。
static BUILTIN_SKILLS: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/skills");

#[derive(Clone)]
pub struct SkillCatalog {
    /// name → skill（后源覆盖前源）。
    pub skills: HashMap<String, Skill>,
}

impl SkillCatalog {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    /// 从一个源目录加载（递归找 SKILL.md）。
    pub fn load_dir(&mut self, dir: &Path, source: &'static str) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.load_dir(&path, source);
            } else if path.file_name().map(|n| n == "SKILL.md").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some(skill) = parse_skill(&content, source) {
                        // 后源覆盖前源。
                        self.skills.insert(skill.name.clone(), skill);
                    }
                }
            }
        }
    }

    /// 加载内置 skills（从 include_dir 嵌入）。
    pub fn load_builtin(&mut self) {
        load_builtin_recursive(&BUILTIN_SKILLS, "builtin", &mut self.skills);
    }

    /// 检索。
    pub fn search(&self, query: &str) -> Vec<SkillHit> {
        let skills: Vec<Skill> = self.skills.values().cloned().collect();
        bm25::search(&skills, query)
    }

    /// 按 name 取 skill body。
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn list(&self) -> Vec<&Skill> {
        let mut v: Vec<&Skill> = self.skills.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }
}

impl Default for SkillCatalog {
    fn default() -> Self {
        Self::new()
    }
}

fn load_builtin_recursive(
    dir: &include_dir::Dir,
    source: &'static str,
    out: &mut HashMap<String, Skill>,
) {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(d) => load_builtin_recursive(d, source, out),
            include_dir::DirEntry::File(f) => {
                if f.path()
                    .file_name()
                    .map(|n| n == "SKILL.md")
                    .unwrap_or(false)
                {
                    if let Ok(content) = std::str::from_utf8(f.contents()) {
                        if let Some(skill) = parse_skill(content, source) {
                            out.insert(skill.name.clone(), skill);
                        }
                    }
                }
            }
        }
    }
}

/// 首跑：把内置 skills 复制到 global 目录（不覆盖已存在）。
pub fn seed_builtin_to_global(global_dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(global_dir) {
        tracing::warn!(error = %e, "seed: create global skills dir failed");
        return;
    }
    seed_dir_recursive(&BUILTIN_SKILLS, global_dir);
}

fn seed_dir_recursive(dir: &include_dir::Dir, dest: &Path) {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(d) => {
                let sub = dest.join(d.path().file_name().unwrap_or_default());
                let _ = std::fs::create_dir_all(&sub);
                seed_dir_recursive(d, &sub);
            }
            include_dir::DirEntry::File(f) => {
                let target = dest.join(f.path());
                if target.exists() {
                    continue; // 不覆盖
                }
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&target, f.contents());
            }
        }
    }
}

/// 解析 Skill 的 body 为对 LLM 友好的注入文本。
pub fn render_skill_body(skill: &Skill) -> String {
    format!("# Skill: {}\n\n{}", skill.name, skill.body)
}

/// 全局 skills 目录：<data_dir>/skills。
pub fn global_skills_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("skills")
}

/// 项目级 skills 目录：<workspace>/.deepseek-science/skills。
pub fn project_skills_dir(workspace: &Path) -> PathBuf {
    workspace.join(".deepseek-science").join("skills")
}
