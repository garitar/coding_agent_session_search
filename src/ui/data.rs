use crate::model::types::{Conversation, Message, MessageRole, Workspace};
use crate::storage::sqlite::SqliteStorage;
use crate::ui::components::theme::ThemePalette;
use anyhow::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    Query,
    Agent,
    Workspace,
    CreatedFrom,
    CreatedTo,
}

#[derive(Clone, Debug)]
pub struct ConversationView {
    pub convo: Conversation,
    pub messages: Vec<Message>,
    pub workspace: Option<Workspace>,
}

pub fn load_conversation(
    storage: &SqliteStorage,
    source_path: &str,
) -> Result<Option<ConversationView>> {
    let mut stmt = storage.raw().prepare(
        "SELECT c.id, a.slug, w.id, w.path, w.display_name, c.external_id, c.title, c.source_path, c.started_at, c.ended_at, c.approx_tokens, c.metadata_json
         FROM conversations c
         JOIN agents a ON c.agent_id = a.id
         LEFT JOIN workspaces w ON c.workspace_id = w.id
         WHERE c.source_path = ?1
         ORDER BY c.started_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([source_path])?;
    if let Some(row) = rows.next()? {
        let convo_id: i64 = row.get(0)?;
        let convo = Conversation {
            id: Some(convo_id),
            agent_slug: row.get(1)?,
            workspace: row
                .get::<_, Option<String>>(3)?
                .map(std::path::PathBuf::from),
            external_id: row.get(5)?,
            title: row.get(6)?,
            source_path: std::path::PathBuf::from(row.get::<_, String>(7)?),
            started_at: row.get(8)?,
            ended_at: row.get(9)?,
            approx_tokens: row.get(10)?,
            metadata_json: row
                .get::<_, Option<String>>(11)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            messages: Vec::new(),
        };
        let workspace = row.get::<_, Option<i64>>(2)?.map(|id| Workspace {
            id: Some(id),
            path: convo.workspace.clone().unwrap_or_default(),
            display_name: row.get(4).ok().flatten(),
        });
        let messages = storage.fetch_messages(convo_id)?;
        return Ok(Some(ConversationView {
            convo,
            messages,
            workspace,
        }));
    }
    Ok(None)
}

pub fn role_style(role: &MessageRole, palette: ThemePalette) -> ratatui::style::Style {
    use ratatui::style::Style;
    match role {
        MessageRole::User => Style::default().fg(palette.user),
        MessageRole::Agent => Style::default().fg(palette.agent),
        MessageRole::Tool => Style::default().fg(palette.tool),
        MessageRole::System => Style::default().fg(palette.system),
        MessageRole::Other(_) => Style::default().fg(palette.hint),
    }
}
