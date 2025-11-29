use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use coding_agent_search::connectors::{Connector, ScanContext, codex::CodexConnector};

#[test]
fn codex_connector_reads_modern_envelope_jsonl() {
    let dir = TempDir::new().unwrap();
    let sessions = dir.path().join("sessions/2025/11/21");
    fs::create_dir_all(&sessions).unwrap();
    let file = sessions.join("rollout-1.jsonl");

    // Modern envelope format with {type, timestamp, payload}
    let sample = r#"{"timestamp":"2025-09-30T15:42:34.559Z","type":"session_meta","payload":{"id":"test-id","cwd":"/test/workspace","cli_version":"0.42.0"}}
{"timestamp":"2025-09-30T15:42:36.190Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"write a hello program"}]}}
{"timestamp":"2025-09-30T15:42:43.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"text","text":"here is code"}]}}
"#;
    fs::write(&file, sample).unwrap();

    // Safe in test scope: we control process env.
    unsafe {
        std::env::set_var("CODEX_HOME", dir.path());
    }

    let connector = CodexConnector::new();
    let ctx = ScanContext {
        data_root: dir.path().to_path_buf(),
        since_ts: None,
    };
    let convs = connector.scan(&ctx).unwrap();
    assert_eq!(convs.len(), 1);
    let c = &convs[0];
    assert_eq!(c.agent_slug, "codex");
    assert_eq!(c.messages.len(), 2);
    assert!(c.title.as_ref().unwrap().contains("write a hello program"));
    // Verify workspace was extracted from session_meta
    assert_eq!(c.workspace, Some(PathBuf::from("/test/workspace")));
    // Verify timestamps were parsed from ISO-8601
    assert!(c.started_at.is_some());
    assert!(c.ended_at.is_some());
}

#[test]
fn codex_connector_includes_agent_reasoning() {
    let dir = TempDir::new().unwrap();
    let sessions = dir.path().join("sessions/2025/11/22");
    fs::create_dir_all(&sessions).unwrap();
    let file = sessions.join("rollout-reasoning.jsonl");

    let sample = r#"{"timestamp":"2025-09-30T15:42:34.559Z","type":"session_meta","payload":{"id":"test-id","cwd":"/test"}}
{"timestamp":"2025-09-30T15:42:36.190Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"solve this problem"}]}}
{"timestamp":"2025-09-30T15:42:40.000Z","type":"event_msg","payload":{"type":"agent_reasoning","text":"Let me think about this carefully..."}}
{"timestamp":"2025-09-30T15:42:43.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"text","text":"here is solution"}]}}
{"timestamp":"2025-09-30T15:42:45.000Z","type":"event_msg","payload":{"type":"token_count","input_tokens":100,"output_tokens":200}}
"#;
    fs::write(&file, sample).unwrap();

    unsafe {
        std::env::set_var("CODEX_HOME", dir.path());
    }

    let connector = CodexConnector::new();
    let ctx = ScanContext {
        data_root: dir.path().to_path_buf(),
        since_ts: None,
    };
    let convs = connector.scan(&ctx).unwrap();
    assert_eq!(convs.len(), 1);
    let c = &convs[0];

    // Should have 3 messages: user, reasoning, assistant
    // (token_count is filtered out)
    assert_eq!(c.messages.len(), 3);

    // Check reasoning is included - find by payload type in extra field
    let reasoning = c
        .messages
        .iter()
        .find(|m| {
            m.extra
                .get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str())
                == Some("agent_reasoning")
        });
    assert!(reasoning.is_some(), "should include agent_reasoning message");
    assert!(
        reasoning
            .unwrap()
            .content
            .contains("think about this carefully")
    );
}

#[test]
fn codex_connector_filters_token_count() {
    let dir = TempDir::new().unwrap();
    let sessions = dir.path().join("sessions/2025/11/23");
    fs::create_dir_all(&sessions).unwrap();
    let file = sessions.join("rollout-filter.jsonl");

    let sample = r#"{"timestamp":"2025-09-30T15:42:34.559Z","type":"session_meta","payload":{"id":"test-id","cwd":"/test"}}
{"timestamp":"2025-09-30T15:42:36.190Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}
{"timestamp":"2025-09-30T15:42:37.000Z","type":"event_msg","payload":{"type":"token_count","input_tokens":10,"output_tokens":20}}
{"timestamp":"2025-09-30T15:42:38.000Z","type":"turn_context","payload":{"turn":1}}
{"timestamp":"2025-09-30T15:42:39.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"text","text":"world"}]}}
"#;
    fs::write(&file, sample).unwrap();

    unsafe {
        std::env::set_var("CODEX_HOME", dir.path());
    }

    let connector = CodexConnector::new();
    let ctx = ScanContext {
        data_root: dir.path().to_path_buf(),
        since_ts: None,
    };
    let convs = connector.scan(&ctx).unwrap();
    assert_eq!(convs.len(), 1);
    let c = &convs[0];

    // Should only have 2 messages (user, assistant)
    // token_count and turn_context should be filtered out
    assert_eq!(c.messages.len(), 2);

    for msg in &c.messages {
        assert!(!msg.content.contains("token_count"));
        assert!(!msg.content.contains("turn_context"));
        assert!(!msg.content.trim().is_empty());
    }
}
