//! Opt-in live smoke test for the free public DeepWiki MCP service.
//!
//! This is ignored in ordinary CI because it depends on a third-party network service. Run with:
//! `cargo test --locked -p dss-mcp --test deepwiki_live_e2e -- --ignored --nocapture`

use dss_mcp::client::PROTOCOL_VERSION;
use dss_mcp::MCPClient;

const DEEPWIKI_MCP_URL: &str = "https://mcp.deepwiki.com/mcp";

#[tokio::test]
#[ignore = "live anonymous DeepWiki MCP request"]
async fn public_repository_structure_round_trip() {
    let client = MCPClient::new(DEEPWIKI_MCP_URL);
    client.connect().await.expect("initialize DeepWiki MCP");

    let metadata = client.metadata().expect("negotiated server metadata");
    assert_eq!(metadata.protocol_version, PROTOCOL_VERSION);
    assert_eq!(metadata.server_name, "DeepWiki");
    assert!(metadata.capabilities.tools);

    let tools = client.list_tools().await.expect("list DeepWiki tools");
    assert_eq!(tools.len(), 3, "all public DeepWiki tools should mount");
    let ask = tools
        .iter()
        .find(|tool| tool.name == "ask_question")
        .expect("DeepWiki ask_question tool");
    assert_eq!(ask.input_schema["properties"]["repoName"]["type"], "string");
    assert!(ask.input_schema["properties"]["repoName"]
        .get("anyOf")
        .is_none());

    let structure = tools
        .iter()
        .find(|tool| tool.name == "read_wiki_structure")
        .expect("DeepWiki read_wiki_structure tool");
    assert_eq!(structure.input_schema["type"], "object");

    let output = client
        .call_tool(
            "read_wiki_structure",
            serde_json::json!({"repoName": "modelcontextprotocol/modelcontextprotocol"}),
        )
        .await
        .expect("read public repository wiki structure");
    assert!(!output.trim().is_empty());
    let lower = output.to_ascii_lowercase();
    assert!(
        lower.contains("model context protocol")
            || lower.contains("modelcontextprotocol")
            || lower.contains("architecture"),
        "unexpected DeepWiki output: {}",
        output.chars().take(500).collect::<String>()
    );
    println!(
        "DeepWiki live MCP passed: protocol={}, server={}, tools={}, output_bytes={}",
        metadata.protocol_version,
        metadata.server_name,
        tools.len(),
        output.len()
    );
}
