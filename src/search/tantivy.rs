use std::path::Path;

use anyhow::{Result, anyhow};
use tantivy::schema::*;
use tantivy::{Index, IndexReader, IndexWriter, doc};

use crate::connectors::NormalizedConversation;

const SCHEMA_VERSION: &str = "v4";

// Bump this when schema/tokenizer changes. Used to trigger rebuilds.
pub const SCHEMA_HASH: &str = "tantivy-schema-v4-edge-ngram-preview";

#[derive(Clone, Copy)]
pub struct Fields {
    pub agent: Field,
    pub workspace: Field,
    pub source_path: Field,
    pub msg_idx: Field,
    pub created_at: Field,
    pub title: Field,
    pub content: Field,
    pub title_prefix: Field,
    pub content_prefix: Field,
    pub preview: Field,
}

pub struct TantivyIndex {
    pub index: Index,
    writer: IndexWriter,
    pub fields: Fields,
}

impl TantivyIndex {
    pub fn open_or_create(path: &Path) -> Result<Self> {
        let schema = build_schema();
        std::fs::create_dir_all(path)?;

        let meta_path = path.join("schema_hash.json");
        let mut needs_rebuild = true;
        if meta_path.exists() {
            let meta = std::fs::read_to_string(&meta_path)?;
            if meta.contains(SCHEMA_HASH) {
                needs_rebuild = false;
            }
        }

        if needs_rebuild {
            // Recreate index directory completely to avoid stale lock files.
            let _ = std::fs::remove_dir_all(path);
            std::fs::create_dir_all(path)?;
        }

        let mut index = if path.join("meta.json").exists() && !needs_rebuild {
            Index::open_in_dir(path)?
        } else {
            Index::create_in_dir(path, schema.clone())?
        };

        ensure_tokenizer(&mut index);

        std::fs::write(
            &meta_path,
            format!("{{\"schema_hash\":\"{}\"}}", SCHEMA_HASH),
        )?;

        let writer = index
            .writer(50_000_000)
            .map_err(|e| anyhow!("create index writer: {e:?}"))?;
        let fields = fields_from_schema(&schema)?;
        Ok(Self {
            index,
            writer,
            fields,
        })
    }

    pub fn add_conversation(&mut self, conv: &NormalizedConversation) -> Result<()> {
        self.add_messages(conv, &conv.messages)
    }

    pub fn delete_all(&mut self) -> Result<()> {
        self.writer.delete_all_documents()?;
        Ok(())
    }

    pub fn commit(&mut self) -> Result<()> {
        self.writer.commit()?;
        Ok(())
    }

    pub fn reader(&self) -> Result<IndexReader> {
        Ok(self.index.reader()?)
    }

    pub fn add_messages(
        &mut self,
        conv: &NormalizedConversation,
        messages: &[crate::connectors::NormalizedMessage],
    ) -> Result<()> {
        for msg in messages {
            let mut d = doc! {
                self.fields.agent => conv.agent_slug.clone(),
                self.fields.source_path => conv.source_path.to_string_lossy().into_owned(),
                self.fields.msg_idx => msg.idx as u64,
                self.fields.content => msg.content.clone(),
            };
            if let Some(ws) = &conv.workspace {
                d.add_text(self.fields.workspace, ws.to_string_lossy());
            }
            if let Some(ts) = msg.created_at.or(conv.started_at) {
                d.add_i64(self.fields.created_at, ts);
            }
            if let Some(title) = &conv.title {
                d.add_text(self.fields.title, title);
                d.add_text(self.fields.title_prefix, generate_edge_ngrams(title));
            }
            d.add_text(
                self.fields.content_prefix,
                generate_edge_ngrams(&msg.content),
            );
            d.add_text(self.fields.preview, build_preview(&msg.content, 200));
            self.writer.add_document(d)?;
        }
        Ok(())
    }
}

fn generate_edge_ngrams(text: &str) -> String {
    let mut ngrams = String::with_capacity(text.len() * 2);
    // Split by non-alphanumeric characters to identify words
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let chars: Vec<char> = word.chars().collect();
        if chars.len() < 2 {
            continue;
        }
        // Generate edge ngrams of length 2..=20 (or word length)
        for len in 2..=chars.len().min(20) {
            if !ngrams.is_empty() {
                ngrams.push(' ');
            }
            ngrams.extend(chars[0..len].iter());
        }
    }
    ngrams
}

pub fn build_schema() -> Schema {
    let mut schema_builder = Schema::builder();
    let text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("hyphen_normalize")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();

    let text_not_stored = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("hyphen_normalize")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );

    schema_builder.add_text_field("agent", TEXT | STORED);
    schema_builder.add_text_field("workspace", STRING | STORED);
    schema_builder.add_text_field("source_path", STORED);
    schema_builder.add_u64_field("msg_idx", INDEXED | STORED);
    schema_builder.add_i64_field("created_at", INDEXED | STORED | FAST);
    schema_builder.add_text_field("title", text.clone());
    schema_builder.add_text_field("content", text);
    schema_builder.add_text_field("title_prefix", text_not_stored.clone());
    schema_builder.add_text_field("content_prefix", text_not_stored);
    schema_builder.add_text_field("preview", TEXT | STORED);
    schema_builder.build()
}

pub fn fields_from_schema(schema: &Schema) -> Result<Fields> {
    let get = |name: &str| {
        schema
            .get_field(name)
            .map_err(|_| anyhow!("schema missing {}", name))
    };
    Ok(Fields {
        agent: get("agent")?,
        workspace: get("workspace")?,
        source_path: get("source_path")?,
        msg_idx: get("msg_idx")?,
        created_at: get("created_at")?,
        title: get("title")?,
        content: get("content")?,
        title_prefix: get("title_prefix")?,
        content_prefix: get("content_prefix")?,
        preview: get("preview")?,
    })
}

fn build_preview(content: &str, max_chars: usize) -> String {
    let char_count = content.chars().count();
    if char_count <= max_chars {
        return content.to_string();
    }
    let mut out = String::new();
    for ch in content.chars().take(max_chars) {
        out.push(ch);
    }
    out.push('…');
    out
}

pub fn index_dir(base: &Path) -> Result<std::path::PathBuf> {
    let dir = base.join("index").join(SCHEMA_VERSION);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn ensure_tokenizer(index: &mut Index) {
    use tantivy::tokenizer::{LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer};
    let analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(LowerCaser)
        .filter(RemoveLongFilter::limit(40))
        .build();
    index.tokenizers().register("hyphen_normalize", analyzer);
}
