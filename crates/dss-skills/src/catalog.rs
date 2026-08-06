//! Skill 目录 + 多源加载。
//!
//! P5a 源：builtin（include_dir! 嵌入）→ global(data_dir/skills) → project(workspace/.dss/skills)。
//! 首跑复制 builtin 到 global（不覆盖）。claude/custom 源留 P5b。
//! 后源覆盖前源（同名 skill 取后加载的）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::bm25;
use crate::frontmatter::parse_skill;
use crate::skill::{Skill, SkillHit};

/// 嵌入的内置 skills（编译期打进二进制）。
static BUILTIN_SKILLS: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/skills");

#[derive(Clone, Default)]
pub struct SkillCatalog {
    /// name → skill（后源覆盖前源）。
    pub skills: HashMap<String, Skill>,
    /// 被禁用的 skill 名称：仍保留在 `skills` 中（供设置 UI 展示），但不进入 agent 的检索/读取。
    pub disabled: HashSet<String>,
}

impl SkillCatalog {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            disabled: HashSet::new(),
        }
    }

    /// 设置禁用的 skill 名称集合。
    pub fn set_disabled<I: IntoIterator<Item = String>>(&mut self, names: I) {
        self.disabled = names.into_iter().collect();
    }

    /// 该 skill 是否启用（未被禁用）。
    pub fn is_enabled(&self, name: &str) -> bool {
        !self.disabled.contains(name)
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

    /// 检索（仅启用的 skill）。
    pub fn search(&self, query: &str) -> Vec<SkillHit> {
        let skills: Vec<Skill> = self
            .skills
            .values()
            .filter(|s| self.is_enabled(&s.name))
            .cloned()
            .collect();
        bm25::search(&skills, query)
    }

    /// 按 name 取 skill body（禁用的 skill 视为不存在）。
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name).filter(|_| self.is_enabled(name))
    }

    /// 启用的 skill 列表（agent 可见）。
    pub fn list(&self) -> Vec<&Skill> {
        let mut v: Vec<&Skill> = self
            .skills
            .values()
            .filter(|s| self.is_enabled(&s.name))
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// 全部 skill 列表（含被禁用项），供设置 UI 展示与开关。
    pub fn list_all(&self) -> Vec<&Skill> {
        let mut v: Vec<&Skill> = self.skills.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
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

/// 用户 home 目录（跨平台兜底）。
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Claude Code skills 目录：`~/.claude/skills`。
pub fn claude_skills_dirs() -> Vec<PathBuf> {
    home_dir()
        .map(|h| vec![h.join(".claude").join("skills")])
        .unwrap_or_default()
}

/// Codex skills 目录：`~/.codex/skills`。
pub fn codex_skills_dirs() -> Vec<PathBuf> {
    home_dir()
        .map(|h| vec![h.join(".codex").join("skills")])
        .unwrap_or_default()
}

/// Cursor skills 目录：`~/.cursor/skills-cursor` 与 `~/.cursor/skills`。
pub fn cursor_skills_dirs() -> Vec<PathBuf> {
    home_dir()
        .map(|h| {
            vec![
                h.join(".cursor").join("skills-cursor"),
                h.join(".cursor").join("skills"),
            ]
        })
        .unwrap_or_default()
}

/// 按配置从多源构建 catalog。
///
/// 源顺序（后源覆盖同名前源）：builtin → global → claude → codex → cursor → custom。
/// `disabled` 中的 skill 仍会被加载（供 UI 展示），但通过 [`SkillCatalog::is_enabled`] 屏蔽。
pub fn build_catalog(
    global_dir: &Path,
    include_claude: bool,
    include_codex: bool,
    include_cursor: bool,
    custom_dirs: &[PathBuf],
    disabled: &[String],
) -> SkillCatalog {
    let mut catalog = SkillCatalog::new();
    catalog.load_builtin();
    catalog.load_dir(global_dir, "global");
    if include_claude {
        for dir in claude_skills_dirs() {
            catalog.load_dir(&dir, "claude");
        }
    }
    if include_codex {
        for dir in codex_skills_dirs() {
            catalog.load_dir(&dir, "codex");
        }
    }
    if include_cursor {
        for dir in cursor_skills_dirs() {
            catalog.load_dir(&dir, "cursor");
        }
    }
    for dir in custom_dirs {
        catalog.load_dir(dir, "custom");
    }
    catalog.set_disabled(disabled.iter().cloned());
    catalog
}
