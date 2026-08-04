//! SKILL.md frontmatter 解析（modules.md §5）。
//!
//! 格式：`---\n<yaml 顶层键值>\n---\n<body>`。只读顶层（跳过缩进行，避免 `metadata:` 块遮蔽 `name`）。
//! 约束：SKILL_MAX_BYTES=65536、DESCRIPTION_MAX=1024、NAME_RE=^[a-z0-9\-/]+$。

use crate::skill::Skill;

/// SKILL.md 单文件最大字节数。
pub const SKILL_MAX_BYTES: usize = 65_536;
/// description 最大字符数。
pub const DESCRIPTION_MAX: usize = 1024;
/// name 正则（简化：只校验字符集，不引 regex crate）。
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '/')
}

/// 解析一段 SKILL.md 文本（含 frontmatter + body）。返回 Skill（不带 source）。
///
/// 失败（无 frontmatter / name 缺失 / name 非法 / 超长）返回 None。
pub fn parse_skill(content: &str, source: &str) -> Option<Skill> {
    if content.len() > SKILL_MAX_BYTES {
        tracing::warn!(source, "skill exceeds SKILL_MAX_BYTES, skipped");
        return None;
    }
    let content = content.trim_start_matches('\u{feff}');
    // 必须以 `---` 起始。
    let after = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    // 找闭合 `---`（独占一行）。
    let end = find_frontmatter_end(after)?;
    let fm = &after[..end];
    let body_start = after[end..]
        .trim_start_matches("---")
        .trim_start_matches(['\n', '\r']);
    let body = body_start.to_string();

    // 只读顶层键值（跳过缩进行）。
    let mut name = None;
    let mut description = None;
    for line in fm.lines() {
        // 缩进行（属于嵌套块，如 metadata:）跳过。
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        if let Some((k, v)) = split_kv(line) {
            let key = k.trim();
            let val = v.trim().trim_matches('"').trim_matches('\'');
            match key {
                "name" => name = Some(val.to_string()),
                "description" => {
                    let d = if val.chars().count() > DESCRIPTION_MAX {
                        val.chars().take(DESCRIPTION_MAX).collect::<String>()
                    } else {
                        val.to_string()
                    };
                    description = Some(d);
                }
                _ => {}
            }
        }
    }

    let name = name?;
    if !is_valid_name(&name) {
        tracing::warn!(source, name = %name, "invalid skill name, skipped");
        return None;
    }
    Some(Skill {
        name,
        description: description.unwrap_or_default(),
        source: source.to_string(),
        body,
    })
}

/// 在 frontmatter 体里找独占一行的 `---`（结束标记）。返回其起始字节偏移。
fn find_frontmatter_end(s: &str) -> Option<usize> {
    for (idx, line) in s.lines().enumerate() {
        if line.trim() == "---" {
            // 计算到该行起始的偏移。
            let mut off = 0usize;
            for (i, l) in s.lines().enumerate() {
                if i == idx {
                    return Some(off);
                }
                off += l.len() + 1; // +1 for \n（lines 不含分隔符）
            }
        }
    }
    None
}

fn split_kv(line: &str) -> Option<(&str, &str)> {
    let col = line.find(':')?;
    let (k, v) = line.split_at(col);
    Some((k, &v[1..])) // 跳过 ':'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_skill() {
        let md = "---\nname: my-skill\ndescription: does a thing\n---\n# body\ncontent here\n";
        let s = parse_skill(md, "test").expect("parse");
        assert_eq!(s.name, "my-skill");
        assert_eq!(s.description, "does a thing");
        assert!(s.body.contains("# body"));
        assert_eq!(s.source, "test");
    }

    #[test]
    fn skips_indented_metadata_block() {
        // 缩进的 metadata 块里的 name 不应遮蔽顶层 name。
        let md =
            "---\nname: top\nmetadata:\n  name: should-be-ignored\ndescription: d\n---\nbody\n";
        let s = parse_skill(md, "test").expect("parse");
        assert_eq!(s.name, "top");
    }

    #[test]
    fn rejects_invalid_name() {
        let md = "---\nname: Bad Name!\ndescription: d\n---\nbody\n";
        assert!(parse_skill(md, "test").is_none());
    }

    #[test]
    fn rejects_missing_frontmatter() {
        assert!(parse_skill("no frontmatter here", "test").is_none());
    }

    #[test]
    fn truncates_long_description() {
        let long = "x".repeat(2000);
        let md = format!("---\nname: s\ndescription: {long}\n---\nbody\n");
        let s = parse_skill(&md, "test").expect("parse");
        assert_eq!(s.description.chars().count(), DESCRIPTION_MAX);
    }
}
