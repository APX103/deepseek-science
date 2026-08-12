//! dss-mcp: MCP streamable-HTTP 客户端 + server 管理器。
//!
//! P7：connect/list_tools/call_tool/list_resources/read_resource + manager and mounting.

pub mod client;
pub mod manager;

pub use client::{
    MCPClient, McpError, McpResource, McpResourceContent, McpRouteOptions, McpServerCapabilities,
    McpServerMetadata, McpTool, McpToolAnnotations, MAX_MCP_TOOLS, MAX_MCP_TOOL_LIST_BYTES,
};
pub use manager::{MCPServerManager, ServerInfo};

/// Maximum OpenAI-compatible function-name length.
pub const MCP_TOOL_NAME_MAX_BYTES: usize = 64;

/// 动态挂载的工具名：mcp__{server}__{tool}。
///
/// Existing names that already match `[A-Za-z0-9_-]+` and fit the model API's
/// 64-byte limit are preserved exactly. Any normalization or truncation gets a
/// stable tuple fingerprint, so distinct remote names such as `a/b` and `a b`
/// cannot collapse to the same model-visible function name.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    let exact = format!("mcp__{server}__{tool}");
    if exact.len() <= MCP_TOOL_NAME_MAX_BYTES
        && exact.bytes().all(is_tool_name_byte)
        && !server.contains("__")
        && !tool.contains("__")
    {
        return exact;
    }

    let normalized_server = normalize_tool_name_segment(server);
    let normalized_tool = normalize_tool_name_segment(tool);
    let mut normalized = format!("mcp__{normalized_server}__{normalized_tool}");
    let suffix = format!("__h{:016x}", tool_name_fingerprint(server, tool));
    let head_bytes = MCP_TOOL_NAME_MAX_BYTES - suffix.len();
    normalized.truncate(normalized.len().min(head_bytes));
    normalized.push_str(&suffix);
    normalized
}

fn normalize_tool_name_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii() && is_tool_name_byte(character as u8) {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn is_tool_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn tool_name_fingerprint(server: &str, tool: &str) -> u64 {
    // Stable FNV-1a over a length-delimited tuple. Length prefixes avoid the same separator
    // ambiguity that the human-readable `mcp__server__tool` portion intentionally retains for
    // backwards compatibility.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in server
        .len()
        .to_le_bytes()
        .into_iter()
        .chain(server.bytes())
        .chain(tool.len().to_le_bytes())
        .chain(tool.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_valid_dynamic_tool_names_are_unchanged() {
        assert_eq!(mcp_tool_name("search", "echo"), "mcp__search__echo");
    }

    #[test]
    fn invalid_characters_are_normalized_with_distinct_stable_fingerprints() {
        let slash = mcp_tool_name("fixture", "a/b");
        let space = mcp_tool_name("fixture", "a b");
        assert_ne!(slash, space);
        assert_eq!(slash, mcp_tool_name("fixture", "a/b"));
        for name in [slash, space, mcp_tool_name("服务器", "查询 数据")] {
            assert!(name.len() <= MCP_TOOL_NAME_MAX_BYTES);
            assert!(name.bytes().all(is_tool_name_byte));
            assert!(name.contains("__h"));
        }
    }

    #[test]
    fn overlong_names_are_bounded_and_fingerprinted() {
        let name = mcp_tool_name(&"s".repeat(128), &"t".repeat(256));
        assert_eq!(name.len(), MCP_TOOL_NAME_MAX_BYTES);
        assert!(name.bytes().all(is_tool_name_byte));
        assert!(name.contains("__h"));
    }

    #[test]
    fn separator_ambiguous_tuples_receive_distinct_fingerprints() {
        let left = mcp_tool_name("alpha__beta", "gamma");
        let right = mcp_tool_name("alpha", "beta__gamma");
        assert_ne!(left, right);
        assert!(left.contains("__h"));
        assert!(right.contains("__h"));
    }
}
