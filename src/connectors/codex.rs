use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use walkdir::WalkDir;

use crate::connectors::{
    Connector, DetectionResult, NormalizedConversation, NormalizedMessage, ScanContext,
};

pub struct CodexConnector;
impl Default for CodexConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexConnector {
    pub fn new() -> Self {
        Self
    }

    fn home() -> PathBuf {
        std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".codex"))
    }

    fn rollout_files(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let sessions = root.join("sessions");
        if !sessions.exists() {
            return out;
        }
        for entry in WalkDir::new(sessions).into_iter().flatten() {
            if entry.file_type().is_file() {
                let name = entry.file_name().to_str().unwrap_or("");
                // Match both modern .jsonl and legacy .json formats
                if name.starts_with("rollout-")
                    && (name.ends_with(".jsonl") || name.ends_with(".json"))
                {
                    out.push(entry.path().to_path_buf());
                }
            }
        }
        out
    }
}

impl Connector for CodexConnector {
    fn detect(&self) -> DetectionResult {
        let home = Self::home();
        if home.join("sessions").exists() {
            DetectionResult {
                detected: true,
                evidence: vec![format!("found {}", home.display())],
            }
        } else {
            DetectionResult::not_found()
        }
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<NormalizedConversation>> {
        // Use data_root only if it looks like a Codex home directory (for testing)
        // Otherwise use the default home
        let home = if ctx.data_root.join("sessions").exists()
            || ctx
                .data_root
                .file_name()
                .map(|n| n.to_str().unwrap_or("").contains("codex"))
                .unwrap_or(false)
        {
            ctx.data_root.clone()
        } else {
            Self::home()
        };
        let files = Self::rollout_files(&home);
        let mut convs = Vec::new();

        for file in files {
            let source_path = file.clone();
            let external_id = source_path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            let content = fs::read_to_string(&file)
                .with_context(|| format!("read rollout {}", file.display()))?;

            let ext = file.extension().and_then(|e| e.to_str());
            let mut messages = Vec::new();
            let mut started_at = None;
            let mut ended_at = None;
            let mut session_cwd: Option<PathBuf> = None;

            if ext == Some("jsonl") {
                // Modern envelope format: each line has {type, timestamp, payload}
                // Pre-scan to find the first model from turn_context (before any messages)
                let mut current_model: Option<String> = None;
                for line in content.lines() {
                    if let Ok(val) = serde_json::from_str::<Value>(line) {
                        if val.get("type").and_then(|v| v.as_str()) == Some("turn_context") {
                            if let Some(model) = val.get("payload")
                                .and_then(|p| p.get("model"))
                                .and_then(|m| m.as_str())
                            {
                                current_model = Some(model.to_string());
                                break; // Found the model, stop scanning
                            }
                        }
                    }
                }

                for line in content.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let val: Value = match serde_json::from_str(line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    let entry_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let created = val
                        .get("timestamp")
                        .and_then(crate::connectors::parse_timestamp);

                    if let (Some(since), Some(ts)) = (ctx.since_ts, created)
                        && ts <= since
                    {
                        continue;
                    }

                    match entry_type {
                        "session_meta" => {
                            // Extract workspace from session metadata
                            if let Some(payload) = val.get("payload") {
                                session_cwd = payload
                                    .get("cwd")
                                    .and_then(|v| v.as_str())
                                    .map(PathBuf::from);
                                // Don't use model_provider as fallback - it's just "openai"
                                // The actual model comes from turn_context entries
                            }
                            started_at = started_at.or(created);
                        }
                        "turn_context" => {
                            // Extract model from turn context - this has the actual model name
                            if let Some(payload) = val.get("payload") {
                                if let Some(model) = payload.get("model").and_then(|v| v.as_str()) {
                                    current_model = Some(model.to_string());
                                }
                            }
                        }
                        "response_item" => {
                            // Main message entries with nested payload
                            if let Some(payload) = val.get("payload") {
                                let payload_type = payload.get("type").and_then(|v| v.as_str());

                                // Determine role: "tool" for function_call/function_call_output,
                                // otherwise explicit role or infer from payload type
                                let role = match payload_type {
                                    Some("function_call") | Some("function_call_output") => "tool",
                                    _ => payload
                                        .get("role")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or_else(|| {
                                            // Infer role from payload type for messages without explicit role
                                            match payload_type {
                                                Some("reasoning") => "assistant",
                                                Some("message") => "assistant",
                                                _ => "assistant",
                                            }
                                        }),
                                };

                                // Extract content from various field names:
                                // - "content" for message/reasoning types
                                // - "output" for function_call_output types
                                // - "name" + "arguments" for function_call types
                                let content_str = if payload_type == Some("function_call") {
                                    // For function calls, combine name and arguments
                                    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                                    let args = payload.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                                    format!("[Tool: {}] {}", name, args)
                                } else {
                                    payload
                                        .get("content")
                                        .or_else(|| payload.get("output"))
                                        .map(crate::connectors::flatten_content)
                                        .unwrap_or_default()
                                };

                                if content_str.trim().is_empty() {
                                    continue;
                                }

                                started_at = started_at.or(created);
                                ended_at = created.or(ended_at);

                                // author = None for user, model for assistant
                                let author = if role == "user" {
                                    None
                                } else {
                                    current_model.clone()
                                };

                                messages.push(NormalizedMessage {
                                    idx: 0, // will be re-assigned after filtering
                                    role: role.to_string(),
                                    author,
                                    model: current_model.clone(),
                                    created_at: created,
                                    content: content_str,
                                    extra: val,
                                    snippets: Vec::new(),
                                });
                            }
                        }
                        // Skip event_msg entirely - all content duplicates response_item:
                        // - user_message duplicates response_item with role=user
                        // - agent_message duplicates response_item with role=assistant
                        // - agent_reasoning duplicates response_item with type=reasoning
                        _ => {}
                    }
                }
                crate::connectors::finalize_messages(&mut messages);
            } else if ext == Some("json") {
                // Legacy format: single JSON object with {session, items}
                let val: Value = match serde_json::from_str(&content) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Extract workspace from session.cwd
                session_cwd = val
                    .get("session")
                    .and_then(|s| s.get("cwd"))
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);

                // Parse items array
                if let Some(items) = val.get("items").and_then(|v| v.as_array()) {
                    for item in items.iter() {
                        let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("assistant");

                        let content_str = item
                            .get("content")
                            .map(crate::connectors::flatten_content)
                            .unwrap_or_default();

                        if content_str.trim().is_empty() {
                            continue;
                        }

                        let created = item
                            .get("timestamp")
                            .and_then(crate::connectors::parse_timestamp);

                        if let (Some(since), Some(ts)) = (ctx.since_ts, created)
                            && ts <= since
                        {
                            continue;
                        }

                        started_at = started_at.or(created);
                        ended_at = created.or(ended_at);

                        messages.push(NormalizedMessage {
                            idx: 0, // will be re-assigned after filtering
                            role: role.to_string(),
                            author: None,
                            model: None, // TODO: extract if available in legacy format
                            created_at: created,
                            content: content_str,
                            extra: item.clone(),
                            snippets: Vec::new(),
                        });
                    }
                }
                crate::connectors::finalize_messages(&mut messages);
            }

            if messages.is_empty() {
                continue;
            }

            // Extract title from first user message
            let title = messages
                .iter()
                .find(|m| m.role == "user")
                .map(|m| {
                    m.content
                        .lines()
                        .next()
                        .unwrap_or(&m.content)
                        .chars()
                        .take(100)
                        .collect::<String>()
                })
                .or_else(|| {
                    messages
                        .first()
                        .and_then(|m| m.content.lines().next())
                        .map(|s| s.chars().take(100).collect())
                });

            convs.push(NormalizedConversation {
                agent_slug: "codex".to_string(),
                external_id,
                title,
                workspace: session_cwd, // Now populated from session_meta/session.cwd!
                source_path: source_path.clone(),
                started_at,
                ended_at,
                metadata: serde_json::json!({"source": if ext == Some("json") { "rollout_json" } else { "rollout" }}),
                messages,
            });
        }

        Ok(convs)
    }
}
