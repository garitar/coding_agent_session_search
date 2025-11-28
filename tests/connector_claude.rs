use coding_agent_search::connectors::claude_code::ClaudeCodeConnector;
use coding_agent_search::connectors::{Connector, ScanContext};
use std::path::PathBuf;

#[test]
fn claude_parses_project_fixture() {
    // Setup isolated environment with "claude" in path to satisfy detector
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture_src =
        PathBuf::from("tests/fixtures/claude_code_real/projects/-test-project/agent-test123.jsonl");
    let fixture_dest_dir = tmp.path().join("mock-claude/projects/test-project");
    std::fs::create_dir_all(&fixture_dest_dir).unwrap();
    let fixture_dest = fixture_dest_dir.join("agent-test123.jsonl");
    std::fs::copy(&fixture_src, &fixture_dest).expect("copy fixture");

    // Run scan on temp dir
    let conn = ClaudeCodeConnector::new();
    let ctx = ScanContext {
        data_root: tmp.path().join("mock-claude"),
        since_ts: None,
    };
    let convs = conn.scan(&ctx).expect("scan");
    assert_eq!(convs.len(), 1);

    let c = &convs[0];
    assert!(!c.title.as_deref().unwrap_or("").is_empty());
    assert_eq!(c.messages.len(), 2);
    assert_eq!(c.messages[1].role, "assistant");
    assert!(c.messages[1].content.contains("matrix completion"));

    // Verify metadata extraction
    let meta = &c.metadata;
    assert_eq!(
        meta.get("sessionId").and_then(|v| v.as_str()),
        Some("test-session")
    );
    assert_eq!(meta.get("gitBranch").and_then(|v| v.as_str()), Some("main"));
}
