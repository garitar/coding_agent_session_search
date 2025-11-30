//! Normalized entity structs.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Roles seen across source agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Agent,
    Tool,
    System,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: Option<i64>,
    pub slug: String,
    pub name: String,
    pub version: Option<String>,
    pub kind: AgentKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentKind {
    Cli,
    VsCode,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Option<i64>,
    pub path: PathBuf,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Option<i64>,
    pub agent_slug: String,
    pub workspace: Option<PathBuf>,
    pub external_id: Option<String>,
    pub title: Option<String>,
    pub source_path: PathBuf,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub approx_tokens: Option<i64>,
    pub metadata_json: serde_json::Value,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Option<i64>,
    pub idx: i64,
    pub role: MessageRole,
    /// Who wrote this message (None for user, model name for assistant)
    pub author: Option<String>,
    /// The AI model active in this conversation (for all messages)
    pub model: Option<String>,
    pub created_at: Option<i64>,
    pub content: String,
    pub extra_json: serde_json::Value,
    pub snippets: Vec<Snippet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub id: Option<i64>,
    pub file_path: Option<PathBuf>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub language: Option<String>,
    pub snippet_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: Option<i64>,
    pub name: String,
}
