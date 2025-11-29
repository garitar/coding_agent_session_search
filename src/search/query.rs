use anyhow::Result;
use lru::LruCache;
use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tantivy::collector::TopDocs;
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, RangeQuery, TermQuery};
use tantivy::schema::{IndexRecordOption, Term, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::{Index, IndexReader, Opstamp, TantivyDocument};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use rusqlite::Connection;

use crate::search::tantivy::fields_from_schema;

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub agents: HashSet<String>,
    pub workspaces: HashSet<String>,
    pub created_from: Option<i64>,
    pub created_to: Option<i64>,
    /// Filter by message role (e.g., "user", "assistant")
    pub role: Option<String>,
    /// Filter by model name (supports glob patterns: "opus" → contains, "gpt-5*" → prefix, "*opus" → suffix, "\"exact\"" → exact)
    pub models: Vec<String>,
}

/// A conversation turn included as context around a search hit
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextTurn {
    /// The role of this turn (e.g., "user", "assistant")
    pub role: String,
    /// The content of this turn
    pub content: String,
    /// The position of this turn in the conversation (0-indexed)
    pub turn_index: i64,
    /// Timestamp of this turn (milliseconds since epoch)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// The author of this turn - "User" for user, model name for assistant
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Whether this is the matched turn (the search hit itself)
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_match: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub title: String,
    pub snippet: String,
    pub content: String,
    pub score: f32,
    pub source_path: String,
    pub agent: String,
    pub workspace: String,
    /// The role of this message (e.g., "user", "assistant")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The author of this message - "User" for user messages, model name for assistant
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub created_at: Option<i64>,
    /// Line number in the source file where the matched message starts (1-indexed)
    pub line_number: Option<usize>,
    /// Internal message ID (used for context fetching)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    /// Internal conversation ID (used for context fetching)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<i64>,
    /// Surrounding conversation turns (populated when --turns is used)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<ContextTurn>>,
}

pub struct SearchClient {
    reader: Option<(IndexReader, crate::search::tantivy::Fields)>,
    sqlite: Option<Connection>,
    prefix_cache: Mutex<CacheShards>,
    last_reload: Mutex<Option<Instant>>,
    last_opstamp: Mutex<Option<Opstamp>>,
    warm_tx: Option<mpsc::UnboundedSender<WarmJob>>,
    _warm_handle: Option<JoinHandle<()>>,
    // Shared for warm worker to read cache/filter logic; keep Arc to avoid clones of big data
    _shared_filters: Arc<Mutex<()>>, // placeholder lock to ensure Send/Sync; future warm prefill state
    metrics: Metrics,
}

// Cache tuning: read from env to allow runtime override without recompiling.
// CASS_CACHE_SHARD_CAP controls per-shard entries; default 256.
static CACHE_SHARD_CAP: Lazy<usize> = Lazy::new(|| {
    std::env::var("CASS_CACHE_SHARD_CAP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(256)
});

// Warm debounce (ms) for background reload/warm jobs; default 120ms.
static WARM_DEBOUNCE_MS: Lazy<u64> = Lazy::new(|| {
    std::env::var("CASS_WARM_DEBOUNCE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(120)
});

#[derive(Clone)]
struct CachedHit {
    hit: SearchHit,
    lc_content: String,
    lc_title: Option<String>,
    lc_snippet: String,
    bloom64: u64,
}

#[derive(Default)]
struct CacheShards {
    shards: HashMap<String, LruCache<String, Vec<CachedHit>>>,
}

impl CacheShards {
    fn shard_mut(&mut self, name: &str) -> &mut LruCache<String, Vec<CachedHit>> {
        self.shards
            .entry(name.to_string())
            .or_insert_with(|| LruCache::new(NonZeroUsize::new(*CACHE_SHARD_CAP).unwrap()))
    }

    fn shard_opt(&self, name: &str) -> Option<&LruCache<String, Vec<CachedHit>>> {
        self.shards.get(name)
    }
}

#[derive(Clone)]
struct WarmJob {
    query: String,
    _filters: SearchFilters,
}

fn sanitize_query(raw: &str) -> String {
    // Replace any character that is not alphanumeric with a space.
    // This ensures that the input tokens match how SimpleTokenizer splits content.
    // e.g. "c++" -> "c  ", "foo.bar" -> "foo bar"
    raw.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect()
}

/// Check if content is primarily a tool invocation (noise that shouldn't appear in search results).
/// Tool invocations like "[Tool: Bash - Check status]" are not informative search results.
fn is_tool_invocation_noise(content: &str) -> bool {
    let trimmed = content.trim();

    // Direct tool invocations that are just "[Tool: X - description]"
    if trimmed.starts_with("[Tool:") {
        // If it's short or ends with ']', it's pure noise
        if trimmed.len() < 100 || trimmed.ends_with(']') {
            return true;
        }
    }

    // Also filter very short content that's just tool names or markers
    if trimmed.len() < 20 {
        let lower = trimmed.to_lowercase();
        if lower.starts_with("[tool") || lower.starts_with("tool:") {
            return true;
        }
    }

    false
}

/// Deduplicate search hits by content, keeping only the highest-scored hit for each unique content.
/// This removes duplicate results when the same message appears multiple times (e.g., user repeated
/// themselves in a conversation, or the same content was indexed from multiple sources).
/// Also filters out tool invocation noise that isn't useful for search results.
fn deduplicate_hits(hits: Vec<SearchHit>) -> Vec<SearchHit> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut deduped: Vec<SearchHit> = Vec::new();

    for hit in hits {
        // Skip tool invocation noise
        if is_tool_invocation_noise(&hit.content) {
            continue;
        }

        // Normalize content for comparison (trim whitespace, collapse multiple spaces)
        let normalized = hit.content.split_whitespace().collect::<Vec<_>>().join(" ");

        if let Some(&existing_idx) = seen.get(&normalized) {
            // If existing hit has lower score, replace it
            if deduped[existing_idx].score < hit.score {
                deduped[existing_idx] = hit;
            }
            // Otherwise keep existing (higher score)
        } else {
            seen.insert(normalized, deduped.len());
            deduped.push(hit);
        }
    }

    deduped
}

impl SearchClient {
    pub fn open(index_path: &Path, db_path: Option<&Path>) -> Result<Option<Self>> {
        let tantivy = Index::open_in_dir(index_path).ok().and_then(|mut idx| {
            // Register custom tokenizer so searches work
            crate::search::tantivy::ensure_tokenizer(&mut idx);
            let schema = idx.schema();
            let fields = fields_from_schema(&schema).ok()?;
            idx.reader().ok().map(|reader| (reader, fields))
        });

        let sqlite = db_path.and_then(|p| Connection::open(p).ok());

        if tantivy.is_none() && sqlite.is_none() {
            return Ok(None);
        }

        let shared_filters = Arc::new(Mutex::new(()));

        let warm_pair = if let Some((reader, fields)) = &tantivy {
            maybe_spawn_warm_worker(reader.clone(), *fields, Arc::downgrade(&shared_filters))
        } else {
            None
        };

        Ok(Some(Self {
            reader: tantivy,
            sqlite,
            prefix_cache: Mutex::new(CacheShards::default()),
            last_reload: Mutex::new(None),
            last_opstamp: Mutex::new(None),
            warm_tx: warm_pair.as_ref().map(|(tx, _)| tx.clone()),
            _warm_handle: warm_pair.map(|(_, h)| h),
            _shared_filters: shared_filters,
            metrics: Metrics::default(),
        }))
    }

    pub fn search(
        &self,
        query: &str,
        filters: SearchFilters,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchHit>> {
        let sanitized = sanitize_query(query);

        // Schedule warmup for likely prefixes when user pauses typing.
        if offset == 0
            && let Some(tx) = &self.warm_tx
        {
            let _ = tx.send(WarmJob {
                query: sanitized.clone(),
                _filters: filters.clone(),
            });
        }

        // Fast path: reuse cached prefix when user is typing forward (offset 0 only).
        if offset == 0 {
            if let Some(cached) = self.cached_prefix_hits(&sanitized, &filters) {
                let mut filtered: Vec<SearchHit> = cached
                    .into_iter()
                    .filter(|h| hit_matches_query_cached(h, &sanitized))
                    .map(|c| c.hit.clone())
                    .collect();
                if filtered.len() >= limit {
                    filtered.truncate(limit);
                    self.metrics.inc_cache_hits();
                    return Ok(filtered);
                }
                self.metrics.inc_cache_shortfall();
            }
            self.metrics.inc_cache_miss();
        }

        // Tantivy is the primary high-performance engine.
        if let Some((reader, fields)) = &self.reader {
            tracing::info!(
                backend = "tantivy",
                query = sanitized,
                limit = limit,
                offset = offset,
                "search_start"
            );
            let hits = self.search_tantivy(
                reader,
                fields,
                &sanitized,
                filters.clone(),
                limit * 3,
                offset,
            )?;
            if !hits.is_empty() {
                let mut deduped = deduplicate_hits(hits);
                deduped.truncate(limit);
                self.put_cache(&sanitized, &filters, &deduped);
                return Ok(deduped);
            }
            // If Tantivy yields 0 results, we can optionally fall back to SQLite FTS
            // if we suspect consistency issues, but for now let's trust Tantivy
            // or fall through if you prefer robust fallback.
            // Given the "speed first" requirement, we return early if we got hits.
            // If empty, we *can* try SQLite just in case index is lagging.
        }

        // Fallback: SQLite FTS (slower, but strictly consistent with DB)
        if let Some(conn) = &self.sqlite {
            tracing::info!(
                backend = "sqlite",
                query = sanitized,
                limit = limit,
                offset = offset,
                "search_start"
            );
            let hits = self.search_sqlite(conn, &sanitized, filters.clone(), limit * 3, offset)?;
            let mut deduped = deduplicate_hits(hits);
            deduped.truncate(limit);
            self.put_cache(&sanitized, &filters, &deduped);
            return Ok(deduped);
        }

        tracing::info!(backend = "none", query = query, "search_start");
        Ok(Vec::new())
    }

    fn search_tantivy(
        &self,
        reader: &IndexReader,
        fields: &crate::search::tantivy::Fields,
        query: &str,
        filters: SearchFilters,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchHit>> {
        self.maybe_reload_reader(reader)?;
        let searcher = reader.searcher();

        // Invalidate cache if index generation changed
        if let Ok(meta) = searcher.index().load_metas() {
            let current_opstamp = meta.opstamp;
            let mut last_op_guard = self.last_opstamp.lock().unwrap();
            if let Some(last) = *last_op_guard
                && last != current_opstamp
                && let Ok(mut cache) = self.prefix_cache.lock()
            {
                cache.shards.clear();
            }
            *last_op_guard = Some(current_opstamp);
        }

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

        // Manual query construction for "search-as-you-type" prefix support.
        // We treat each whitespace-separated token as a MUST clause.
        // Each token matches if it appears in title OR content OR their prefix variants.
        let terms: Vec<&str> = query.split_whitespace().collect();
        if !terms.is_empty() {
            for term_str in terms {
                let term_lower = term_str.to_lowercase();
                // Match in title, content, or their prefix (edge n-gram) variants
                let term_shoulds: Vec<(Occur, Box<dyn Query>)> = vec![
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(fields.title, &term_lower),
                            IndexRecordOption::WithFreqsAndPositions,
                        )),
                    ),
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(fields.content, &term_lower),
                            IndexRecordOption::WithFreqsAndPositions,
                        )),
                    ),
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(fields.title_prefix, &term_lower),
                            IndexRecordOption::WithFreqsAndPositions,
                        )),
                    ),
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(fields.content_prefix, &term_lower),
                            IndexRecordOption::WithFreqsAndPositions,
                        )),
                    ),
                ];
                clauses.push((Occur::Must, Box::new(BooleanQuery::new(term_shoulds))));
            }
        } else {
            clauses.push((Occur::Must, Box::new(AllQuery)));
        }

        if !filters.agents.is_empty() {
            let terms = filters
                .agents
                .into_iter()
                .map(|agent| {
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(fields.agent, &agent),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    )
                })
                .collect();
            clauses.push((Occur::Must, Box::new(BooleanQuery::new(terms))));
        }

        if !filters.workspaces.is_empty() {
            let terms = filters
                .workspaces
                .into_iter()
                .map(|ws| {
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(fields.workspace, &ws),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    )
                })
                .collect();
            clauses.push((Occur::Must, Box::new(BooleanQuery::new(terms))));
        }

        if filters.created_from.is_some() || filters.created_to.is_some() {
            use std::ops::Bound::{Included, Unbounded};
            let lower = filters
                .created_from
                .map(|v| Included(Term::from_field_i64(fields.created_at, v)))
                .unwrap_or(Unbounded);
            let upper = filters
                .created_to
                .map(|v| Included(Term::from_field_i64(fields.created_at, v)))
                .unwrap_or(Unbounded);
            let range = RangeQuery::new(lower, upper);
            clauses.push((Occur::Must, Box::new(range)));
        }

        let q: Box<dyn Query> = if clauses.is_empty() {
            Box::new(AllQuery)
        } else if clauses.len() == 1 {
            clauses.pop().unwrap().1
        } else {
            Box::new(BooleanQuery::new(clauses))
        };

        let prefix_only = is_prefix_only(query);
        let snippet_generator = if prefix_only {
            None
        } else {
            Some(SnippetGenerator::create(&searcher, &*q, fields.content)?)
        };

        let top_docs = searcher.search(&q, &TopDocs::with_limit(limit).and_offset(offset))?;
        let mut hits = Vec::new();
        for (score, addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let title = doc
                .get_first(fields.title)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = doc
                .get_first(fields.content)
                .or_else(|| doc.get_first(fields.preview))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let agent = doc
                .get_first(fields.agent)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = if let Some(r#gen) = &snippet_generator {
                r#gen
                    .snippet_from_doc(&doc)
                    .to_html()
                    .replace("<b>", "**")
                    .replace("</b>", "**")
            } else if let Some(sn) = cached_prefix_snippet(&content, query, 160) {
                sn
            } else {
                quick_prefix_snippet(&content, query, 160)
            };
            let source = doc
                .get_first(fields.source_path)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let workspace = doc
                .get_first(fields.workspace)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let created_at = doc.get_first(fields.created_at).and_then(|v| v.as_i64());
            hits.push(SearchHit {
                title,
                snippet,
                content,
                score,
                source_path: source,
                agent,
                workspace,
                role: None,  // Not in Tantivy index; enriched from SQLite later
                author: None, // Not in Tantivy index; enriched from SQLite later
                created_at,
                line_number: None, // TODO: populate from index if stored
                message_id: None,  // Not available from Tantivy index
                conversation_id: None,
                context: None,
            });
        }
        Ok(hits)
    }

    fn search_sqlite(
        &self,
        conn: &Connection,
        query: &str,
        filters: SearchFilters,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchHit>> {
        // FTS5 cannot handle empty queries
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            "SELECT f.title, f.content, f.agent, f.workspace, f.source_path, f.created_at, bm25(fts_messages) AS score, snippet(fts_messages, 0, '**', '**', '...', 64) AS snippet, m.idx, f.message_id, m.conversation_id, m.author, m.role
             FROM fts_messages f
             LEFT JOIN messages m ON f.message_id = m.id
             WHERE fts_messages MATCH ?",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(query.to_string())];

        if !filters.agents.is_empty() {
            let placeholders = (0..filters.agents.len())
                .map(|_| "?".to_string())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND f.agent IN ({placeholders})"));
            for a in filters.agents {
                params.push(Box::new(a));
            }
        }

        if !filters.workspaces.is_empty() {
            let placeholders = (0..filters.workspaces.len())
                .map(|_| "?".to_string())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND f.workspace IN ({placeholders})"));
            for w in filters.workspaces {
                params.push(Box::new(w));
            }
        }

        if filters.created_from.is_some() {
            sql.push_str(" AND f.created_at >= ?");
            params.push(Box::new(filters.created_from.unwrap()));
        }
        if filters.created_to.is_some() {
            sql.push_str(" AND f.created_at <= ?");
            params.push(Box::new(filters.created_to.unwrap()));
        }
        if let Some(ref role) = filters.role {
            sql.push_str(" AND m.role = ?");
            params.push(Box::new(role.clone()));
        }

        sql.push_str(" ORDER BY score LIMIT ? OFFSET ?");
        params.push(Box::new(limit as i64));
        params.push(Box::new(offset as i64));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params.iter().map(|b| &**b)),
            |row| {
                let title: String = row.get(0)?;
                let content: String = row.get(1)?;
                let agent: String = row.get(2)?;
                let workspace: String = row.get(3)?;
                let source_path: String = row.get(4)?;
                let created_at: Option<i64> = row.get(5).ok();
                let score: f32 = row.get::<_, f64>(6)? as f32;
                let snippet: String = row.get(7)?;
                // idx is 0-indexed message index; convert to 1-indexed line number for JSONL files
                let idx: Option<i64> = row.get(8).ok();
                let line_number = idx.map(|i| (i + 1) as usize);
                let message_id: Option<i64> = row.get(9).ok();
                let conversation_id: Option<i64> = row.get(10).ok();
                let author: Option<String> = row.get(11).ok();
                let role: Option<String> = row.get(12).ok();
                Ok(SearchHit {
                    title,
                    snippet,
                    content,
                    score,
                    source_path,
                    agent,
                    workspace,
                    role,
                    author,
                    created_at,
                    line_number,
                    message_id,
                    conversation_id,
                    context: None,
                })
            },
        )?;

        let mut hits = Vec::new();
        for row in rows {
            hits.push(row?);
        }
        Ok(hits)
    }

    /// Enrich a SearchHit with message_id, conversation_id, role, and model from SQLite.
    /// This is used when Tantivy search finds results but we need context.
    pub fn enrich_hit_for_context(&self, hit: &mut SearchHit) -> Result<()> {
        let conn = self.sqlite.as_ref().ok_or_else(|| {
            anyhow::anyhow!("SQLite connection not available for hit enrichment")
        })?;

        // Look up the message by content (first 500 chars to handle truncation)
        let content_prefix: String = hit.content.chars().take(500).collect();
        let sql = "SELECT m.id, m.conversation_id, m.idx, m.role, m.author FROM messages m
                   WHERE m.content LIKE ? || '%'
                   LIMIT 1";

        let mut stmt = conn.prepare(sql)?;
        let result = stmt.query_row([&content_prefix], |row| {
            let id: i64 = row.get(0)?;
            let conversation_id: i64 = row.get(1)?;
            let idx: i64 = row.get(2)?;
            let role: Option<String> = row.get(3).ok();
            let author: Option<String> = row.get(4).ok();
            Ok((id, conversation_id, idx, role, author))
        });

        if let Ok((id, conv_id, idx, role, author)) = result {
            hit.message_id = Some(id);
            hit.conversation_id = Some(conv_id);
            hit.line_number = Some((idx + 1) as usize);
            if hit.role.is_none() {
                hit.role = role;
            }
            if hit.author.is_none() {
                hit.author = author;
            }
        }
        Ok(())
    }

    /// Fetch surrounding conversation turns for a given message.
    /// A "turn" is defined as a user message + all subsequent assistant messages.
    /// Returns N user turns before and after the matched message.
    pub fn fetch_surrounding_turns(
        &self,
        conversation_id: i64,
        match_idx: i64,
        turns_before: usize,
        turns_after: usize,
    ) -> Result<Vec<ContextTurn>> {
        let conn = self.sqlite.as_ref().ok_or_else(|| {
            anyhow::anyhow!("SQLite connection not available for context fetching")
        })?;

        // Find the user message that starts the turn containing our match
        // (the most recent user message at or before match_idx)
        let match_turn_start: i64 = conn
            .query_row(
                "SELECT idx FROM messages
                 WHERE conversation_id = ? AND idx <= ? AND role = 'user'
                 ORDER BY idx DESC LIMIT 1",
                rusqlite::params![conversation_id, match_idx],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Find the Nth user message before the match turn to get start boundary
        let start_idx: i64 = if turns_before > 0 {
            conn.query_row(
                "SELECT idx FROM messages
                 WHERE conversation_id = ? AND idx < ? AND role = 'user'
                 ORDER BY idx DESC
                 LIMIT 1 OFFSET ?",
                rusqlite::params![
                    conversation_id,
                    match_turn_start,
                    turns_before.saturating_sub(1) as i64
                ],
                |row| row.get(0),
            )
            .unwrap_or(match_turn_start)
        } else {
            match_turn_start
        };

        // Find the Nth user message after the match to get end boundary
        let end_user_idx: Option<i64> = if turns_after > 0 {
            conn.query_row(
                "SELECT idx FROM messages
                 WHERE conversation_id = ? AND idx > ? AND role = 'user'
                 ORDER BY idx ASC
                 LIMIT 1 OFFSET ?",
                rusqlite::params![
                    conversation_id,
                    match_idx,
                    turns_after.saturating_sub(1) as i64
                ],
                |row| row.get(0),
            )
            .ok()
        } else {
            None
        };

        // Find where to stop: just before the next user message after end_user_idx
        let end_idx: i64 = if let Some(end_user) = end_user_idx {
            // Include all messages in the end_user's turn (up to next user or end)
            conn.query_row(
                "SELECT idx FROM messages
                 WHERE conversation_id = ? AND idx > ? AND role = 'user'
                 ORDER BY idx ASC LIMIT 1",
                rusqlite::params![conversation_id, end_user],
                |row| row.get(0),
            )
            .unwrap_or(i64::MAX)
        } else {
            // No end boundary, include all remaining messages in the match's turn
            conn.query_row(
                "SELECT idx FROM messages
                 WHERE conversation_id = ? AND idx > ? AND role = 'user'
                 ORDER BY idx ASC LIMIT 1",
                rusqlite::params![conversation_id, match_idx],
                |row| row.get(0),
            )
            .unwrap_or(i64::MAX)
        };

        // Fetch all messages from start to end
        let sql = "SELECT idx, role, content, created_at, author FROM messages
                   WHERE conversation_id = ? AND idx >= ? AND idx < ?
                   ORDER BY idx ASC";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(
            rusqlite::params![conversation_id, start_idx, end_idx],
            |row| {
                let idx: i64 = row.get(0)?;
                let role: String = row.get(1)?;
                let content: String = row.get(2)?;
                let created_at: Option<i64> = row.get(3).ok();
                let author: Option<String> = row.get(4).ok();
                Ok(ContextTurn {
                    turn_index: idx,
                    role,
                    content,
                    created_at,
                    author,
                    is_match: idx == match_idx,
                })
            },
        )?;

        let mut turns = Vec::new();
        for row in rows {
            turns.push(row?);
        }

        Ok(turns)
    }
}

#[derive(Default, Clone)]
struct Metrics {
    cache_hits: Arc<Mutex<u64>>,
    cache_miss: Arc<Mutex<u64>>,
    cache_shortfall: Arc<Mutex<u64>>,
    reloads: Arc<Mutex<u64>>,
}

impl Metrics {
    fn inc_cache_hits(&self) {
        *self.cache_hits.lock().unwrap() += 1;
    }
    fn inc_cache_miss(&self) {
        *self.cache_miss.lock().unwrap() += 1;
    }
    fn inc_cache_shortfall(&self) {
        *self.cache_shortfall.lock().unwrap() += 1;
    }
    fn inc_reload(&self) {
        *self.reloads.lock().unwrap() += 1;
    }

    #[cfg(test)]
    fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            *self.cache_hits.lock().unwrap(),
            *self.cache_miss.lock().unwrap(),
            *self.cache_shortfall.lock().unwrap(),
            *self.reloads.lock().unwrap(),
        )
    }
}

fn maybe_spawn_warm_worker(
    reader: IndexReader,
    fields: crate::search::tantivy::Fields,
    filters_guard: std::sync::Weak<Mutex<()>>,
) -> Option<(mpsc::UnboundedSender<WarmJob>, JoinHandle<()>)> {
    // Only spawn if a Tokio runtime is available (tests may call without one).
    if Handle::try_current().is_err() {
        return None;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<WarmJob>();
    let handle = tokio::spawn(async move {
        // Simple debounce: process at most one warmup every WARM_DEBOUNCE_MS.
        let mut last_run = Instant::now();
        while let Some(job) = rx.recv().await {
            let now = Instant::now();
            if now.duration_since(last_run) < Duration::from_millis(*WARM_DEBOUNCE_MS) {
                continue;
            }
            last_run = now;
            if filters_guard.upgrade().is_none() {
                break;
            }
            let _ = reader.reload();
            // Run a tiny warm search to prefill OS cache and hit the Tantivy reader
            // without allocating full result sets. Limit 1 doc.
            let searcher = reader.searcher();
            let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
            for term_str in job.query.split_whitespace() {
                let term_lower = term_str.to_lowercase();
                let term_shoulds: Vec<(Occur, Box<dyn Query>)> = vec![
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(fields.title, &term_lower),
                            IndexRecordOption::WithFreqsAndPositions,
                        )),
                    ),
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(fields.content, &term_lower),
                            IndexRecordOption::WithFreqsAndPositions,
                        )),
                    ),
                ];
                clauses.push((Occur::Must, Box::new(BooleanQuery::new(term_shoulds))));
            }
            if !clauses.is_empty() {
                let q: Box<dyn Query> = Box::new(BooleanQuery::new(clauses));
                let _ = searcher.search(&q, &TopDocs::with_limit(1));
            }
        }
    });
    Some((tx, handle))
}

fn cached_hit_from(hit: &SearchHit) -> CachedHit {
    let lc_content = hit.content.to_lowercase();
    let lc_title = (!hit.title.is_empty()).then(|| hit.title.to_lowercase());
    let lc_snippet = hit.snippet.to_lowercase();
    let bloom64 = bloom_from_text(&lc_content, &lc_title, &lc_snippet);
    CachedHit {
        hit: hit.clone(),
        lc_content,
        lc_title,
        lc_snippet,
        bloom64,
    }
}

fn bloom_from_text(content: &str, title: &Option<String>, snippet: &str) -> u64 {
    let mut bits = 0u64;
    for token in token_stream(content) {
        bits |= hash_token(token);
    }
    if let Some(t) = title {
        for token in token_stream(t) {
            bits |= hash_token(token);
        }
    }
    for token in token_stream(snippet) {
        bits |= hash_token(token);
    }
    bits
}

fn token_stream(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
}

fn hash_token(tok: &str) -> u64 {
    // Simple 64-bit djb2-style hash mapped to bit position 0..63
    let mut h: u64 = 5381;
    for b in tok.as_bytes() {
        h = ((h << 5).wrapping_add(h)).wrapping_add(*b as u64);
    }
    1u64 << (h % 64)
}

fn hit_matches_query_cached(hit: &CachedHit, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    let tokens: Vec<&str> = token_stream(&q).collect();
    // Bloom gate: all query tokens must have bits set
    for t in &tokens {
        let bit = hash_token(t);
        if hit.bloom64 & bit == 0 {
            return false;
        }
    }

    // Fallback substring checks on lowered fields
    hit.lc_content.contains(&q)
        || hit
            .lc_title
            .as_ref()
            .map(|t: &String| t.contains(&q))
            .unwrap_or(false)
        || hit.lc_snippet.contains(&q)
}

fn is_prefix_only(query: &str) -> bool {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    tokens
        .iter()
        .all(|t| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric()))
}

fn quick_prefix_snippet(content: &str, query: &str, max_chars: usize) -> String {
    let lc_content = content.to_lowercase();
    let lc_query = query.to_lowercase();
    let content_char_count = content.chars().count();
    if let Some(pos) = lc_content.find(&lc_query) {
        // convert byte index to char index
        let start_char = content[..pos].chars().count().saturating_sub(15);
        let snippet: String = content.chars().skip(start_char).take(max_chars).collect();
        // Check if we truncated: snippet covers chars [start_char, start_char + snippet_len)
        let snippet_char_count = snippet.chars().count();
        if start_char + snippet_char_count < content_char_count {
            format!("{snippet}…")
        } else {
            snippet
        }
    } else {
        let snippet: String = content.chars().take(max_chars).collect();
        if content_char_count > max_chars {
            format!("{snippet}…")
        } else {
            snippet
        }
    }
}

fn cached_prefix_snippet(content: &str, query: &str, max_chars: usize) -> Option<String> {
    if query.trim().is_empty() {
        return None;
    }
    let lc_content = content.to_lowercase();
    let lc_query = query.to_lowercase();
    let content_char_count = content.chars().count();
    lc_content.find(&lc_query).map(|pos| {
        let start_char = content[..pos].chars().count().saturating_sub(15);
        let snippet: String = content.chars().skip(start_char).take(max_chars).collect();
        // Check if we truncated: snippet covers chars [start_char, start_char + snippet_len)
        let snippet_char_count = snippet.chars().count();
        if start_char + snippet_char_count < content_char_count {
            format!("{snippet}…")
        } else {
            snippet
        }
    })
}

fn filters_fingerprint(filters: &SearchFilters) -> String {
    let mut parts = Vec::new();
    if !filters.agents.is_empty() {
        let mut v: Vec<_> = filters.agents.iter().cloned().collect();
        v.sort();
        parts.push(format!("a:{:?}", v));
    }
    if !filters.workspaces.is_empty() {
        let mut v: Vec<_> = filters.workspaces.iter().cloned().collect();
        v.sort();
        parts.push(format!("w:{:?}", v));
    }
    if let Some(f) = filters.created_from {
        parts.push(format!("from:{f}"));
    }
    if let Some(t) = filters.created_to {
        parts.push(format!("to:{t}"));
    }
    parts.join("|")
}

impl SearchClient {
    fn maybe_reload_reader(&self, reader: &IndexReader) -> Result<()> {
        const MIN_RELOAD_INTERVAL: Duration = Duration::from_millis(300);
        let now = Instant::now();
        let mut guard = self.last_reload.lock().unwrap();
        if guard
            .map(|t| now.duration_since(t) >= MIN_RELOAD_INTERVAL)
            .unwrap_or(true)
        {
            reader.reload()?;
            *guard = Some(now);
            self.metrics.inc_reload();
        }
        Ok(())
    }

    fn cache_key(&self, query: &str, filters: &SearchFilters) -> String {
        format!("{query}::{}", filters_fingerprint(filters))
    }

    fn shard_name(&self, filters: &SearchFilters) -> String {
        if filters.agents.len() == 1 {
            filters
                .agents
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| "global".into())
        } else {
            "global".into()
        }
    }

    fn cached_prefix_hits(&self, query: &str, filters: &SearchFilters) -> Option<Vec<CachedHit>> {
        if query.is_empty() {
            return None;
        }
        let cache = self.prefix_cache.lock().ok()?;
        let shard_name = self.shard_name(filters);
        let shard = cache.shard_opt(&shard_name)?;
        // Iterate over character boundaries to avoid slicing mid-codepoint.
        let mut byte_indices: Vec<usize> = query.char_indices().map(|(i, _)| i).collect();
        byte_indices.push(query.len());
        for &end in byte_indices.iter().rev() {
            if end == 0 {
                continue;
            }
            let key = self.cache_key(&query[..end], filters);
            if let Some(hits) = shard.peek(&key) {
                return Some(hits.clone());
            }
        }
        None
    }

    fn put_cache(&self, query: &str, filters: &SearchFilters, hits: &[SearchHit]) {
        if query.is_empty() || hits.is_empty() {
            return;
        }
        if let Ok(mut cache) = self.prefix_cache.lock() {
            let shard_name = self.shard_name(filters);
            let key = self.cache_key(query, filters);
            let shard = cache.shard_mut(&shard_name);
            let cached_hits: Vec<CachedHit> = hits.iter().map(cached_hit_from).collect();
            shard.put(key, cached_hits);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{NormalizedConversation, NormalizedMessage, NormalizedSnippet};
    use crate::search::tantivy::TantivyIndex;
    use tempfile::TempDir;

    #[test]
    fn cache_prefix_lookup_handles_utf8_boundaries() {
        let client = SearchClient {
            reader: None,
            sqlite: None,
            prefix_cache: Mutex::new(CacheShards::default()),
            last_reload: Mutex::new(None),
            last_opstamp: Mutex::new(None),
            warm_tx: None,
            _warm_handle: None,
            _shared_filters: Arc::new(Mutex::new(())),
            metrics: Metrics::default(),
        };

        let hits = vec![SearchHit {
            title: "こんにちは".into(),
            snippet: "".into(),
            content: "こんにちは 世界".into(),
            score: 1.0,
            source_path: "p".into(),
            agent: "a".into(),
            workspace: "w".into(),
            role: None,
            author: None,
            created_at: None,
            line_number: None,
            message_id: None,
            conversation_id: None,
            context: None,
        }];

        client.put_cache("こん", &SearchFilters::default(), &hits);

        let cached = client
            .cached_prefix_hits("こんにちは", &SearchFilters::default())
            .unwrap();
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].hit.title, "こんにちは");
    }

    #[test]
    fn bloom_gate_rejects_missing_terms() {
        let hit = SearchHit {
            title: "hello world".into(),
            snippet: "hello world".into(),
            content: "hello world".into(),
            score: 1.0,
            source_path: "p".into(),
            agent: "a".into(),
            workspace: "w".into(),
            role: None,
            author: None,
            created_at: None,
            line_number: None,
            message_id: None,
            conversation_id: None,
            context: None,
        };
        let cached = cached_hit_from(&hit);
        assert!(hit_matches_query_cached(&cached, "hello"));
        assert!(!hit_matches_query_cached(&cached, "missing"));

        let metrics = Metrics::default();
        metrics.inc_cache_hits();
        metrics.inc_cache_miss();
        metrics.inc_cache_shortfall();
        metrics.inc_reload();
        assert_eq!(metrics.snapshot(), (1, 1, 1, 1));
    }

    #[test]
    fn search_returns_results_with_filters_and_pagination() -> Result<()> {
        let dir = TempDir::new()?;
        let mut index = TantivyIndex::open_or_create(dir.path())?;
        let conv = NormalizedConversation {
            agent_slug: "codex".into(),
            external_id: None,
            title: Some("hello world convo".into()),
            workspace: Some(std::path::PathBuf::from("/tmp/workspace")),
            source_path: dir.path().join("rollout-1.jsonl"),
            started_at: Some(1_700_000_000_000),
            ended_at: None,
            metadata: serde_json::json!({}),
            messages: vec![NormalizedMessage {
                idx: 0,
                role: "user".into(),
                author: Some("me".into()),
                created_at: Some(1_700_000_000_000),
                content: "hello rust world".into(),
                extra: serde_json::json!({}),
                snippets: vec![NormalizedSnippet {
                    file_path: None,
                    start_line: None,
                    end_line: None,
                    language: None,
                    snippet_text: None,
                }],
            }],
        };
        index.add_conversation(&conv)?;
        index.commit()?;

        let client = SearchClient::open(dir.path(), None)?.expect("index present");
        let mut filters = SearchFilters::default();
        filters.agents.insert("codex".into());

        let hits = client.search("hello", filters, 10, 0)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].agent, "codex");
        assert!(hits[0].snippet.contains("hello"));
        Ok(())
    }

    #[test]
    fn search_honors_created_range_and_workspace() -> Result<()> {
        let dir = TempDir::new()?;
        let mut index = TantivyIndex::open_or_create(dir.path())?;

        let conv_a = NormalizedConversation {
            agent_slug: "codex".into(),
            external_id: None,
            title: Some("needle one".into()),
            workspace: Some(std::path::PathBuf::from("/ws/a")),
            source_path: dir.path().join("a.jsonl"),
            started_at: Some(10),
            ended_at: None,
            metadata: serde_json::json!({}),
            messages: vec![NormalizedMessage {
                idx: 0,
                role: "user".into(),
                author: None,
                created_at: Some(10),
                content: "alpha needle".into(),
                extra: serde_json::json!({}),
                snippets: vec![NormalizedSnippet {
                    file_path: None,
                    start_line: None,
                    end_line: None,
                    language: None,
                    snippet_text: None,
                }],
            }],
        };
        let conv_b = NormalizedConversation {
            agent_slug: "codex".into(),
            external_id: None,
            title: Some("needle two".into()),
            workspace: Some(std::path::PathBuf::from("/ws/b")),
            source_path: dir.path().join("b.jsonl"),
            started_at: Some(20),
            ended_at: None,
            metadata: serde_json::json!({}),
            messages: vec![NormalizedMessage {
                idx: 0,
                role: "user".into(),
                author: None,
                created_at: Some(20),
                content: "\nneedle second line".into(),
                extra: serde_json::json!({}),
                snippets: vec![NormalizedSnippet {
                    file_path: None,
                    start_line: None,
                    end_line: None,
                    language: None,
                    snippet_text: None,
                }],
            }],
        };
        index.add_conversation(&conv_a)?;
        index.add_conversation(&conv_b)?;
        index.commit()?;

        let client = SearchClient::open(dir.path(), None)?.expect("index present");
        let mut filters = SearchFilters::default();
        filters.workspaces.insert("/ws/b".into());
        filters.created_from = Some(15);
        filters.created_to = Some(25);

        let hits = client.search("needle", filters, 10, 0)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].workspace, "/ws/b");
        assert!(hits[0].snippet.contains("second line"));
        Ok(())
    }

    #[test]
    fn pagination_skips_results() -> Result<()> {
        let dir = TempDir::new()?;
        let mut index = TantivyIndex::open_or_create(dir.path())?;
        for i in 0..3 {
            let conv = NormalizedConversation {
                agent_slug: "codex".into(),
                external_id: None,
                title: Some(format!("doc-{i}")),
                workspace: Some(std::path::PathBuf::from("/ws/p")),
                source_path: dir.path().join(format!("{i}.jsonl")),
                started_at: Some(100 + i),
                ended_at: None,
                metadata: serde_json::json!({}),
                messages: vec![NormalizedMessage {
                    idx: 0,
                    role: "user".into(),
                    author: None,
                    created_at: Some(100 + i),
                    content: "pagination needle".into(),
                    extra: serde_json::json!({}),
                    snippets: vec![NormalizedSnippet {
                        file_path: None,
                        start_line: None,
                        end_line: None,
                        language: None,
                        snippet_text: None,
                    }],
                }],
            };
            index.add_conversation(&conv)?;
        }
        index.commit()?;

        let client = SearchClient::open(dir.path(), None)?.expect("index present");
        let hits = client.search("pagination", SearchFilters::default(), 1, 1)?;
        assert_eq!(hits.len(), 1);
        Ok(())
    }

    #[test]
    fn search_matches_hyphenated_term() -> Result<()> {
        let dir = TempDir::new()?;
        let mut index = TantivyIndex::open_or_create(dir.path())?;
        let conv = NormalizedConversation {
            agent_slug: "codex".into(),
            external_id: None,
            title: Some("cma-es notes".into()),
            workspace: Some(std::path::PathBuf::from("/tmp/workspace")),
            source_path: dir.path().join("rollout-1.jsonl"),
            started_at: Some(1_700_000_000_000),
            ended_at: None,
            metadata: serde_json::json!({}),
            messages: vec![NormalizedMessage {
                idx: 0,
                role: "user".into(),
                author: Some("me".into()),
                created_at: Some(1_700_000_000_000),
                content: "Need CMA-ES strategy and CMA ES variants".into(),
                extra: serde_json::json!({}),
                snippets: vec![NormalizedSnippet {
                    file_path: None,
                    start_line: None,
                    end_line: None,
                    language: None,
                    snippet_text: None,
                }],
            }],
        };
        index.add_conversation(&conv)?;
        index.commit()?;

        let client = SearchClient::open(dir.path(), None)?.expect("index present");
        let hits = client.search("cma-es", SearchFilters::default(), 10, 0)?;
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.to_lowercase().contains("cma"));
        Ok(())
    }

    #[test]
    fn search_matches_prefix_edge_ngram() -> Result<()> {
        let dir = TempDir::new()?;
        let mut index = TantivyIndex::open_or_create(dir.path())?;
        let conv = NormalizedConversation {
            agent_slug: "codex".into(),
            external_id: None,
            title: Some("math logic".into()),
            workspace: Some(std::path::PathBuf::from("/ws/m")),
            source_path: dir.path().join("math.jsonl"),
            started_at: Some(1000),
            ended_at: None,
            metadata: serde_json::json!({}),
            messages: vec![NormalizedMessage {
                idx: 0,
                role: "user".into(),
                author: None,
                created_at: Some(1000),
                content: "please calculate the entropy".into(),
                extra: serde_json::json!({}),
                snippets: vec![],
            }],
        };
        index.add_conversation(&conv)?;
        index.commit()?;

        let client = SearchClient::open(dir.path(), None)?.expect("index present");

        // "cal" should match "calculate"
        let hits = client.search("cal", SearchFilters::default(), 10, 0)?;
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("calculate"));

        // "entr" should match "entropy"
        let hits = client.search("entr", SearchFilters::default(), 10, 0)?;
        assert_eq!(hits.len(), 1);

        Ok(())
    }

    #[test]
    fn search_matches_snake_case() -> Result<()> {
        let dir = TempDir::new()?;
        let mut index = TantivyIndex::open_or_create(dir.path())?;
        let conv = NormalizedConversation {
            agent_slug: "codex".into(),
            external_id: None,
            title: Some("code".into()),
            workspace: None,
            source_path: dir.path().join("c.jsonl"),
            started_at: Some(1),
            ended_at: None,
            metadata: serde_json::json!({}),
            messages: vec![NormalizedMessage {
                idx: 0,
                role: "user".into(),
                author: None,
                created_at: Some(1),
                content: "check the my_variable_name please".into(),
                extra: serde_json::json!({}),
                snippets: vec![],
            }],
        };
        index.add_conversation(&conv)?;
        index.commit()?;

        let client = SearchClient::open(dir.path(), None)?.expect("index present");

        // "vari" should match "variable" inside "my_variable_name"
        let hits = client.search("vari", SearchFilters::default(), 10, 0)?;
        assert_eq!(hits.len(), 1);

        // "my_variable" should match "my_variable_name" (because it splits to "my variable")
        let hits = client.search("my_variable", SearchFilters::default(), 10, 0)?;
        assert_eq!(hits.len(), 1);

        Ok(())
    }

    #[test]
    fn search_matches_symbols_stripped() -> Result<()> {
        let dir = TempDir::new()?;
        let mut index = TantivyIndex::open_or_create(dir.path())?;
        let conv = NormalizedConversation {
            agent_slug: "codex".into(),
            external_id: None,
            title: Some("symbols".into()),
            workspace: None,
            source_path: dir.path().join("s.jsonl"),
            started_at: Some(1),
            ended_at: None,
            metadata: serde_json::json!({}),
            messages: vec![NormalizedMessage {
                idx: 0,
                role: "user".into(),
                author: None,
                created_at: Some(1),
                content: "working with c++ and foo.bar today".into(),
                extra: serde_json::json!({}),
                snippets: vec![],
            }],
        };
        index.add_conversation(&conv)?;
        index.commit()?;

        let client = SearchClient::open(dir.path(), None)?.expect("index present");

        // "c++" -> "c"
        let hits = client.search("c++", SearchFilters::default(), 10, 0)?;
        assert_eq!(hits.len(), 1);

        // "foo.bar" -> "foo", "bar"
        let hits = client.search("foo.bar", SearchFilters::default(), 10, 0)?;
        assert_eq!(hits.len(), 1);

        Ok(())
    }

    #[test]
    fn cache_invalidates_on_new_data() -> Result<()> {
        let dir = TempDir::new()?;
        let mut index = TantivyIndex::open_or_create(dir.path())?;

        // 1. Add initial doc
        let conv1 = NormalizedConversation {
            agent_slug: "codex".into(),
            external_id: None,
            title: Some("first".into()),
            workspace: None,
            source_path: dir.path().join("1.jsonl"),
            started_at: Some(1),
            ended_at: None,
            metadata: serde_json::json!({}),
            messages: vec![NormalizedMessage {
                idx: 0,
                role: "user".into(),
                author: None,
                created_at: Some(1),
                content: "apple banana".into(),
                extra: serde_json::json!({}),
                snippets: vec![],
            }],
        };
        index.add_conversation(&conv1)?;
        index.commit()?;

        let client = SearchClient::open(dir.path(), None)?.expect("index present");

        // 2. Search "app" -> should hit "apple"
        let hits = client.search("app", SearchFilters::default(), 10, 0)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "apple banana");

        // 3. Verify it's cached (peek internal state)
        {
            let cache = client.prefix_cache.lock().unwrap();
            let shard = cache.shard_opt("global").unwrap();
            // "app" should be in cache
            assert!(shard.contains(&client.cache_key("app", &SearchFilters::default())));
        }

        // 4. Add new doc with "apricot"
        let conv2 = NormalizedConversation {
            agent_slug: "codex".into(),
            external_id: None,
            title: Some("second".into()),
            workspace: None,
            source_path: dir.path().join("2.jsonl"),
            started_at: Some(2),
            ended_at: None,
            metadata: serde_json::json!({}),
            messages: vec![NormalizedMessage {
                idx: 0,
                role: "user".into(),
                author: None,
                created_at: Some(2),
                content: "apricot".into(),
                extra: serde_json::json!({}),
                snippets: vec![],
            }],
        };
        index.add_conversation(&conv2)?;
        index.commit()?;

        // 5. Force reload (mocking time passing or just ensuring reload triggers)
        // In test, maybe_reload_reader uses 300ms debounce.
        // We can rely on opstamp check logic which runs AFTER reload.
        // We need to sleep briefly to bypass debounce or just modify test to not rely on time?
        // Actually SearchClient::maybe_reload_reader checks duration.
        std::thread::sleep(std::time::Duration::from_millis(350));

        // 6. Search "ap" (prefix of apricot and apple)
        // The cache for "app" should be cleared if opstamp changed.
        let _hits = client.search("app", SearchFilters::default(), 10, 0)?;
        // Should now find 1 doc still ("apple"), but cache should have been cleared first

        // Search "apr" -> should find "apricot"
        let hits = client.search("apr", SearchFilters::default(), 10, 0)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "apricot");

        // Check that cache was cleared by verifying a stale key is gone?
        // Or rely on correctness of results if we searched a common prefix?

        Ok(())
    }

    #[test]
    fn context_turn_has_created_at_field() {
        // Verify ContextTurn struct includes created_at for timestamp display
        let turn = ContextTurn {
            role: "user".to_string(),
            content: "Hello world".to_string(),
            turn_index: 0,
            created_at: Some(1700000000000), // Nov 14, 2023
            author: None,
            is_match: true,
        };
        assert_eq!(turn.created_at, Some(1700000000000));

        // Verify it serializes correctly
        let json = serde_json::to_string(&turn).unwrap();
        assert!(json.contains("created_at"));
        assert!(json.contains("1700000000000"));
    }

    #[test]
    fn context_turn_skips_none_created_at_in_json() {
        // Verify skip_serializing_if works for None
        let turn = ContextTurn {
            role: "assistant".to_string(),
            content: "Hi there".to_string(),
            turn_index: 1,
            created_at: None,
            author: None,
            is_match: false,
        };
        let json = serde_json::to_string(&turn).unwrap();
        assert!(!json.contains("created_at"));
        assert!(!json.contains("is_match")); // Also skipped when false
        assert!(!json.contains("model")); // Also skipped when None
    }
}
