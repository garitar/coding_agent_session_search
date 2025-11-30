use coding_agent_search::connectors::{Connector, ScanContext, amp::AmpConnector};
use coding_agent_search::connectors::{NormalizedConversation, NormalizedMessage};
use coding_agent_search::indexer::persist::persist_conversation;
use coding_agent_search::search::query::{SearchClient, SearchFilters};
use coding_agent_search::search::tantivy::{TantivyIndex, index_dir};
use coding_agent_search::storage::sqlite::SqliteStorage;
use tempfile::TempDir;

fn norm_msg(idx: i64) -> NormalizedMessage {
    NormalizedMessage {
        idx,
        role: "user".into(),
        author: None,
        model: None,
        created_at: Some(1_700_000_000_000 + idx),
        content: format!("hello-{idx}"),
        extra: serde_json::json!({}),
        snippets: Vec::new(),
    }
}

#[test]
fn search_logs_backend_selection() {
    let trace = TestTracing::new();
    let _guard = trace.install();

    let dir = TempDir::new().unwrap();
    let mut index = TantivyIndex::open_or_create(dir.path()).unwrap();
    let conv = NormalizedConversation {
        agent_slug: "codex".into(),
        external_id: None,
        title: Some("log test".into()),
        workspace: None,
        source_path: dir.path().join("rollout.jsonl"),
        started_at: Some(1),
        ended_at: Some(2),
        metadata: serde_json::json!({}),
        messages: vec![norm_msg(0)],
    };
    index.add_conversation(&conv).unwrap();
    index.commit().unwrap();

    let client = SearchClient::open(dir.path(), None)
        .unwrap()
        .expect("client");
    client
        .search("hello", SearchFilters::default(), 5, 0)
        .unwrap();

    let out = trace.output();
    eprintln!("logs: {out}");
    assert!(out.contains("backend=\"tantivy\""));
    assert!(out.contains("search_start"));
}

#[test]
fn amp_connector_emits_scan_span() {
    let trace = TestTracing::new();
    let _guard = trace.install();

    let fixture_root = std::path::PathBuf::from("tests/fixtures/amp");
    let conn = AmpConnector::new();
    let ctx = ScanContext {
        data_root: fixture_root,
        since_ts: None,
    };
    let convs = conn.scan(&ctx).unwrap();
    assert!(!convs.is_empty());

    let out = trace.output();
    assert!(out.contains("connector::amp"));
    assert!(out.contains("amp_scan"));
}

#[test]
fn persist_conversation_logs_counts() {
    let trace = TestTracing::new();
    let _guard = trace.install();

    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db_path = data_dir.join("db.sqlite");
    let mut storage = SqliteStorage::open(&db_path).unwrap();
    let mut index = TantivyIndex::open_or_create(&index_dir(&data_dir).unwrap()).unwrap();

    let conv = NormalizedConversation {
        agent_slug: "tester".into(),
        external_id: Some("ext-log".into()),
        title: Some("persist".into()),
        workspace: None,
        source_path: data_dir.join("src.log"),
        started_at: Some(10),
        ended_at: Some(20),
        metadata: serde_json::json!({}),
        messages: vec![norm_msg(0), norm_msg(1)],
    };

    persist_conversation(&mut storage, &mut index, &conv).unwrap();

    let out = trace.output();
    assert!(out.contains("persist_conversation"));
    assert!(out.contains("messages=2"));
}

// Re-export util module so tests can find helpers without extra path noise.
mod util;
use util::TestTracing;
