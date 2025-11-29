pub mod connectors;
pub mod indexer;
pub mod model;
pub mod search;
pub mod storage;
pub mod ui;

use anyhow::Result;
use chrono::Utc;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use indexer::IndexOptions;
use reqwest::Client;
use semver::Version;
use serde::Deserialize;
use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

const CONTRACT_VERSION: &str = "1";

/// Command-line interface.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "cass",
    version,
    about = "Unified TUI search over coding agent histories"
)]
pub struct Cli {
    /// Path to the SQLite database (defaults to platform data dir)
    #[arg(long)]
    pub db: Option<PathBuf>,

    /// Deterministic machine-first help (wide, no TUI)
    #[arg(long, default_value_t = false)]
    pub robot_help: bool,

    /// Trace command execution to JSONL file (spans)
    #[arg(long)]
    pub trace_file: Option<PathBuf>,

    /// Reduce log noise (warnings and errors only)
    #[arg(long, short = 'q', default_value_t = false)]
    pub quiet: bool,

    /// Color behavior for CLI output
    #[arg(long, value_enum, default_value_t = ColorPref::Auto)]
    pub color: ColorPref,

    /// Progress output style
    #[arg(long, value_enum, default_value_t = ProgressMode::Auto)]
    pub progress: ProgressMode,

    /// Wrap informational output to N columns
    #[arg(long)]
    pub wrap: Option<usize>,

    /// Disable wrapping entirely
    #[arg(long, default_value_t = false)]
    pub nowrap: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Launch interactive TUI
    Tui {
        /// Render once and exit (headless-friendly)
        #[arg(long, default_value_t = false)]
        once: bool,

        /// Override data dir (matches index --data-dir)
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Run indexer
    Index {
        /// Perform full rebuild
        #[arg(long)]
        full: bool,

        /// Force Tantivy index rebuild even if schema matches
        #[arg(long, default_value_t = false)]
        force_rebuild: bool,

        /// Watch for changes and reindex automatically
        #[arg(long)]
        watch: bool,

        /// Override data dir (index + db). Defaults to platform data dir.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Generate shell completions to stdout
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Generate man page to stdout
    Man,
    /// Machine-focused docs for automation agents
    RobotDocs {
        /// Topic to print
        #[arg(value_enum)]
        topic: RobotTopic,
    },
    /// Run a one-off search and print results to stdout
    Search {
        /// The query string
        query: String,
        /// Filter by agent slug (can be specified multiple times)
        #[arg(long)]
        agent: Vec<String>,
        /// Filter by workspace path (can be specified multiple times)
        #[arg(long)]
        workspace: Vec<String>,
        /// Max results
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Offset for pagination (start at Nth result)
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Output as JSON (--robot also works)
        #[arg(long, visible_alias = "robot")]
        json: bool,
        /// Override data dir
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Filter to last N days
        #[arg(long)]
        days: Option<u32>,
        /// Filter to today only
        #[arg(long)]
        today: bool,
        /// Filter to yesterday only
        #[arg(long)]
        yesterday: bool,
        /// Filter to last 7 days
        #[arg(long)]
        week: bool,
        /// Filter to entries since ISO date (YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS)
        #[arg(long)]
        since: Option<String>,
        /// Filter to entries until ISO date
        #[arg(long)]
        until: Option<String>,
        /// Include conversation turns around each hit. Can be a single number (N turns before and after)
        /// or "before:after" for asymmetric context (e.g., "2:5" = 2 before, 5 after)
        #[arg(long, short = 'T')]
        turns: Option<String>,
        /// Max characters for snippet/preview in text output (default 200, 0 = full content)
        #[arg(long, short = 'S', default_value = "200")]
        snippet_len: usize,
        /// Hide main content/snippet, show only context turns (use with -T)
        #[arg(long)]
        no_content: bool,
        /// Show model on each turn (by default only shown in headline)
        #[arg(long)]
        full_model: bool,
        /// Filter by message role (user or assistant)
        #[arg(long)]
        from: Option<String>,
        /// Filter by model name (can be specified multiple times).
        /// Supports glob patterns: "opus" matches contains, "gpt*" matches prefix,
        /// "*opus" matches suffix, "\"exact\"" matches exactly.
        #[arg(long)]
        model: Vec<String>,
        /// Show tool calls/results from source file (optional char limit, inherits from -S if not specified, 0 = no limit)
        #[arg(long, num_args = 0..=1, default_missing_value = "18446744073709551615")]
        tools: Option<usize>,
        /// Include context continuation/summary messages (excluded by default)
        #[arg(long)]
        include_summaries: bool,
        /// Include IDE autocontext messages like "# Context from my IDE setup" (excluded by default)
        #[arg(long)]
        include_autocontext: bool,
    },
    /// Show statistics about indexed data
    Stats {
        /// Override data dir
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// View a source file at a specific line (follow up on search results)
    View {
        /// Path to the source file
        path: PathBuf,
        /// Line number to show (1-indexed)
        #[arg(long, short = 'n')]
        line: Option<usize>,
        /// Number of context lines before/after
        #[arg(long, short = 'C', default_value_t = 5)]
        context: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum ColorPref {
    Auto,
    Never,
    Always,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum ProgressMode {
    Auto,
    Bars,
    Plain,
    None,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum RobotTopic {
    Commands,
    Env,
    Paths,
    Schemas,
    ExitCodes,
    Examples,
    Contracts,
    Wrap,
}

#[derive(Debug, Clone)]
pub struct CliError {
    pub code: i32,
    pub kind: &'static str,
    pub message: String,
    pub hint: Option<String>,
    pub retryable: bool,
}

pub type CliResult<T = ()> = std::result::Result<T, CliError>;

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for CliError {}

impl CliError {
    fn usage(message: impl Into<String>, hint: Option<String>) -> Self {
        CliError {
            code: 2,
            kind: "usage",
            message: message.into(),
            hint,
            retryable: false,
        }
    }

    fn unknown(message: impl Into<String>) -> Self {
        CliError {
            code: 9,
            kind: "unknown",
            message: message.into(),
            hint: None,
            retryable: false,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProgressResolved {
    Bars,
    Plain,
    None,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct WrapConfig {
    width: Option<usize>,
    nowrap: bool,
}

impl WrapConfig {
    fn new(width: Option<usize>, nowrap: bool) -> Self {
        WrapConfig { width, nowrap }
    }

    fn effective_width(&self) -> Option<usize> {
        if self.nowrap { None } else { self.width }
    }
}

pub async fn run() -> CliResult<()> {
    let cli = Cli::parse();
    let stdout_is_tty = io::stdout().is_terminal();
    let stderr_is_tty = io::stderr().is_terminal();
    configure_color(cli.color, stdout_is_tty, stderr_is_tty);

    let wrap_cfg = WrapConfig::new(cli.wrap, cli.nowrap);
    let progress_resolved = resolve_progress(cli.progress, stdout_is_tty);

    let start_ts = Utc::now();
    let start_instant = Instant::now();
    let command_label = describe_command(&cli);

    let result = execute_cli(
        &cli,
        wrap_cfg,
        progress_resolved,
        stdout_is_tty,
        stderr_is_tty,
    )
    .await;

    if let Some(path) = &cli.trace_file {
        let duration_ms = start_instant.elapsed().as_millis();
        let exit_code = result.as_ref().map(|_| 0).unwrap_or_else(|e| e.code);
        if let Err(trace_err) = write_trace_line(
            path,
            &command_label,
            &cli,
            &start_ts,
            duration_ms,
            exit_code,
            result.as_ref().err(),
        ) {
            eprintln!("trace-write error: {trace_err}");
        }
    }

    result
}

async fn execute_cli(
    cli: &Cli,
    wrap: WrapConfig,
    progress: ProgressResolved,
    stdout_is_tty: bool,
    stderr_is_tty: bool,
) -> CliResult<()> {
    let command = cli.command.clone().unwrap_or(Commands::Tui {
        once: false,
        data_dir: None,
    });

    if cli.robot_help {
        print_robot_help(wrap)?;
        return Ok(());
    }

    if let Commands::RobotDocs { topic } = command.clone() {
        print_robot_docs(topic, wrap)?;
        return Ok(());
    }

    // Block TUI in non-TTY contexts unless TUI_HEADLESS is set (for testing)
    if matches!(command, Commands::Tui { .. })
        && !stdout_is_tty
        && std::env::var("TUI_HEADLESS").is_err()
    {
        return Err(CliError::usage(
            "No subcommand provided; in non-TTY contexts TUI is disabled.",
            Some("Use an explicit subcommand, e.g., `cass search --json ...` or `cass --robot-help`.".to_string()),
        ));
    }

    let filter = if cli.quiet {
        EnvFilter::new("warn")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };

    match &command {
        Commands::Tui { data_dir, .. } => {
            let log_dir = data_dir.clone().unwrap_or_else(default_data_dir);
            std::fs::create_dir_all(&log_dir).ok();

            let file_appender = tracing_appender::rolling::daily(&log_dir, "cass.log");
            let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .compact()
                        .with_target(false)
                        .with_ansi(false),
                )
                .init();

            maybe_prompt_for_update(matches!(command, Commands::Tui { once: true, .. }))
                .await
                .map_err(|e| CliError {
                    code: 9,
                    kind: "update-check",
                    message: format!("update check failed: {e}"),
                    hint: None,
                    retryable: false,
                })?;

            if let Commands::Tui { once: false, .. } = &command {
                let bg_data_dir = log_dir.clone();
                let bg_db = cli.db.clone();
                // Create shared progress tracker
                let progress = std::sync::Arc::new(indexer::IndexingProgress::default());
                spawn_background_indexer(bg_data_dir, bg_db, Some(progress.clone()));

                if let Commands::Tui { once, data_dir } = command {
                    ui::tui::run_tui(data_dir.clone(), once, Some(progress)).map_err(|e| {
                        CliError {
                            code: 9,
                            kind: "tui",
                            message: format!("tui failed: {e}"),
                            hint: None,
                            retryable: false,
                        }
                    })?;
                }
            } else if let Commands::Tui { once, data_dir } = command {
                ui::tui::run_tui(data_dir.clone(), once, None).map_err(|e| CliError {
                    code: 9,
                    kind: "tui",
                    message: format!("tui failed: {e}"),
                    hint: None,
                    retryable: false,
                })?;
            }
        }
        Commands::Index { .. }
        | Commands::Search { .. }
        | Commands::Stats { .. }
        | Commands::View { .. } => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .compact()
                .with_target(false)
                .with_ansi(
                    matches!(cli.color, ColorPref::Always)
                        || (matches!(cli.color, ColorPref::Auto) && stderr_is_tty),
                )
                .init();

            match command {
                Commands::Index {
                    full,
                    force_rebuild,
                    watch,
                    data_dir,
                } => {
                    run_index_with_data(
                        cli.db.clone(),
                        full,
                        force_rebuild,
                        watch,
                        data_dir,
                        progress,
                    )?;
                }
                Commands::Search {
                    query,
                    agent,
                    workspace,
                    limit,
                    offset,
                    json,
                    data_dir,
                    days,
                    today,
                    yesterday,
                    week,
                    since,
                    until,
                    turns,
                    snippet_len,
                    no_content,
                    full_model,
                    from,
                    model,
                    tools,
                    include_summaries,
                    include_autocontext,
                } => {
                    run_cli_search(
                        &query,
                        &agent,
                        &workspace,
                        &limit,
                        &offset,
                        &json,
                        &data_dir,
                        cli.db.clone(),
                        wrap,
                        progress,
                        TimeFilter::new(
                            days,
                            today,
                            yesterday,
                            week,
                            since.as_deref(),
                            until.as_deref(),
                        ),
                        turns,
                        snippet_len,
                        no_content,
                        full_model,
                        from,
                        model,
                        tools,
                        include_summaries,
                        include_autocontext,
                    )?;
                }
                Commands::Stats { data_dir, json } => {
                    run_stats(&data_dir, cli.db.clone(), json)?;
                }
                Commands::View {
                    path,
                    line,
                    context,
                    json,
                } => {
                    run_view(&path, line, context, json)?;
                }
                _ => {}
            }
        }
        _ => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .compact()
                .with_target(false)
                .with_ansi(
                    matches!(cli.color, ColorPref::Always)
                        || (matches!(cli.color, ColorPref::Auto) && stderr_is_tty),
                )
                .init();

            match command {
                Commands::Completions { shell } => {
                    let mut cmd = Cli::command();
                    clap_complete::generate(shell, &mut cmd, "cass", &mut std::io::stdout());
                }
                Commands::Man => {
                    let cmd = Cli::command();
                    let man = clap_mangen::Man::new(cmd);
                    man.render(&mut std::io::stdout())
                        .map_err(|e| CliError::unknown(format!("failed to render man: {e}")))?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn configure_color(choice: ColorPref, stdout_is_tty: bool, stderr_is_tty: bool) {
    let enabled = match choice {
        ColorPref::Always => true,
        ColorPref::Never => false,
        ColorPref::Auto => stdout_is_tty || stderr_is_tty,
    };
    colored::control::set_override(enabled);
}

fn resolve_progress(mode: ProgressMode, stdout_is_tty: bool) -> ProgressResolved {
    match mode {
        ProgressMode::Bars => ProgressResolved::Bars,
        ProgressMode::Plain => ProgressResolved::Plain,
        ProgressMode::None => ProgressResolved::None,
        ProgressMode::Auto => {
            if stdout_is_tty {
                ProgressResolved::Bars
            } else {
                ProgressResolved::Plain
            }
        }
    }
}

fn describe_command(cli: &Cli) -> String {
    match &cli.command {
        Some(Commands::Tui { .. }) => "tui".to_string(),
        Some(Commands::Index { .. }) => "index".to_string(),
        Some(Commands::Search { .. }) => "search".to_string(),
        Some(Commands::Stats { .. }) => "stats".to_string(),
        Some(Commands::View { .. }) => "view".to_string(),
        Some(Commands::Completions { .. }) => "completions".to_string(),
        Some(Commands::Man) => "man".to_string(),
        Some(Commands::RobotDocs { topic }) => format!("robot-docs:{topic:?}"),
        None => "(default)".to_string(),
    }
}

fn apply_wrap(line: &str, wrap: WrapConfig) -> String {
    let width = wrap.effective_width();
    if line.trim().is_empty() || width.is_none() {
        return line.trim_end().to_string();
    }
    let width = width.unwrap_or(usize::MAX);
    if line.len() <= width {
        return line.trim_end().to_string();
    }

    let mut out = String::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        if current.len() + word.len() + 1 > width && !current.is_empty() {
            out.push_str(current.trim_end());
            out.push('\n');
            current.clear();
        }
        current.push_str(word);
        current.push(' ');
    }
    if !current.is_empty() {
        out.push_str(current.trim_end());
    }
    out
}

fn render_block<T: AsRef<str>>(lines: &[T], wrap: WrapConfig) -> String {
    lines
        .iter()
        .map(|l| apply_wrap(l.as_ref(), wrap))
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_robot_help(wrap: WrapConfig) -> CliResult<()> {
    let lines = vec![
        "cass --robot-help (contract v1)",
        "===============================",
        "",
        "QUICKSTART (for AI agents):",
        "  cass search \"your query\" --robot     # Search with JSON output",
        "  cass search \"bug fix\" --today        # Search today's sessions only",
        "  cass search \"api\" --week --agent codex  # Last 7 days, codex only",
        "  cass stats --json                    # Get index statistics",
        "  cass view /path/file.jsonl -n 42    # View file at line 42",
        "",
        "TIME FILTERS:",
        "  --today | --yesterday | --week | --days N",
        "  --since YYYY-MM-DD | --until YYYY-MM-DD",
        "",
        "WORKFLOW:",
        "  1. cass index --full          # First-time setup (index all sessions)",
        "  2. cass search \"query\" --robot  # Search with JSON output",
        "  3. cass view <source_path> -n <line>  # Follow up on search result",
        "",
        "OUTPUT:",
        "  --robot | --json   Machine-readable JSON output",
        "  stdout=data only; stderr=diagnostics",
        "",
        "Subcommands: search | stats | view | index | tui | robot-docs <topic>",
        "Exit codes: 0 ok; 2 usage; 3 missing index/db; 9 unknown",
        "More: cass robot-docs examples | cass robot-docs commands",
    ];
    println!("{}", render_block(&lines, wrap));
    Ok(())
}

fn print_robot_docs(topic: RobotTopic, wrap: WrapConfig) -> CliResult<()> {
    let lines: Vec<String> = match topic {
        RobotTopic::Commands => vec![
            "commands:".to_string(),
            "  (global) --quiet / -q  Suppress info logs (warnings+errors only)".to_string(),
            "  cass search <query> [OPTIONS]".to_string(),
            "    --agent A         Filter by agent (codex, claude_code, gemini, opencode, amp, cline)".to_string(),
            "    --workspace W     Filter by workspace path".to_string(),
            "    --limit N         Max results (default: 10)".to_string(),
            "    --offset N        Pagination offset (default: 0)".to_string(),
            "    --json | --robot  JSON output for automation".to_string(),
            "    --today           Filter to today only".to_string(),
            "    --yesterday       Filter to yesterday only".to_string(),
            "    --week            Filter to last 7 days".to_string(),
            "    --days N          Filter to last N days".to_string(),
            "    --since DATE      Filter from date (YYYY-MM-DD)".to_string(),
            "    --until DATE      Filter to date (YYYY-MM-DD)".to_string(),
            "    --turns N / -T N  Include N turns before/after each hit (or \"B:A\" for asymmetric)".to_string(),
            "  cass stats [--json] [--data-dir DIR]".to_string(),
            "  cass view <path> [-n LINE] [-C CONTEXT] [--json]".to_string(),
            "  cass index [--full] [--watch] [--data-dir DIR]".to_string(),
            "  cass tui [--once] [--data-dir DIR]".to_string(),
            "  cass robot-docs <topic>".to_string(),
            "  cass --robot-help".to_string(),
        ],
        RobotTopic::Env => vec![
            "env:".to_string(),
            "  CODING_AGENT_SEARCH_NO_UPDATE_PROMPT=1   skip update prompt".to_string(),
            "  TUI_HEADLESS=1                           skip update prompt".to_string(),
            "  CASS_DATA_DIR                            override data dir".to_string(),
            "  CASS_DB_PATH                             override db path".to_string(),
            "  NO_COLOR / CASS_NO_COLOR                 disable color".to_string(),
            "  CASS_TRACE_FILE                          default trace path".to_string(),
        ],
        RobotTopic::Paths => {
            let mut lines: Vec<String> = vec!["paths:".to_string()];
            lines.push(format!("  data dir default: {}", default_data_dir().display()));
            lines.push(format!("  db path default: {}", default_db_path().display()));
            lines.push("  log path: <data-dir>/cass.log (daily rolling)".to_string());
            lines.push("  trace: user-provided path (JSONL).".to_string());
            lines
        }
        RobotTopic::Schemas => vec![
            "schemas:".to_string(),
            "  search: {query:str,limit:int,offset:int,count:int,hits:[{score:f64,agent:str,workspace:str,source_path:str,snippet:str,content:str,title:str,created_at:int?,line_number:int?,context?:[{role:str,content:str,turn_index:int,is_match?:bool}]}]}".to_string(),
            "  error: {error:{code:int,kind:str,message:str,hint:str?,retryable:bool}}".to_string(),
            "  trace: {start_ts:str,end_ts:str,duration_ms:u128,cmd:str,args:[str],exit_code:int,error:?}".to_string(),
        ],
        RobotTopic::ExitCodes => vec![
            "exit-codes:".to_string(),
            " 0 ok | 2 usage | 3 missing index/db | 4 network | 5 data-corrupt | 6 incompatible-version | 7 lock/busy | 8 partial | 9 unknown".to_string(),
        ],
        RobotTopic::Examples => vec![
            "examples:".to_string(),
            "".to_string(),
            "# Basic search with JSON output for agents".to_string(),
            "  cass search \"your query\" --robot".to_string(),
            "".to_string(),
            "# Search with time filters".to_string(),
            "  cass search \"bug\" --today                 # today only".to_string(),
            "  cass search \"api\" --week                  # last 7 days".to_string(),
            "  cass search \"feature\" --days 30           # last 30 days".to_string(),
            "  cass search \"fix\" --since 2025-01-01      # since date".to_string(),
            "  cass search \"error\" --robot --limit 5 --offset 5  # paginate robot output".to_string(),
            "".to_string(),
            "# Filter by agent or workspace".to_string(),
            "  cass search \"error\" --agent codex         # codex sessions only".to_string(),
            "  cass search \"test\" --workspace /myproject # specific project".to_string(),
            "".to_string(),
            "# Follow up on search results".to_string(),
            "  cass view /path/to/session.jsonl -n 42   # view line 42 with context".to_string(),
            "  cass view /path/to/session.jsonl -n 42 -C 10  # 10 lines context".to_string(),
            "".to_string(),
            "# Get index statistics".to_string(),
            "  cass stats --json                        # JSON stats".to_string(),
            "  cass stats                               # Human-readable stats".to_string(),
            "".to_string(),
            "# Full workflow".to_string(),
            "  cass index --full                        # index all sessions".to_string(),
            "  cass search \"cma-es\" --robot             # search".to_string(),
            "  cass view <source_path> -n <line>        # examine result".to_string(),
        ],
        RobotTopic::Contracts => vec![
            "contracts:".to_string(),
            "  stdout data-only; stderr diagnostics/progress.".to_string(),
            "  No implicit TUI when automation flags set or stdout non-TTY.".to_string(),
            "  Color auto off when non-TTY unless forced.".to_string(),
            "  Use --quiet to silence info logs in robot runs.".to_string(),
            "  JSON errors only to stderr.".to_string(),
        ],
        RobotTopic::Wrap => vec![
            "wrap:".to_string(),
            "  Default: no forced wrap (wide output).".to_string(),
            "  --wrap <n>: wrap informational text to n columns.".to_string(),
            "  --nowrap: force no wrapping even if wrap set elsewhere.".to_string(),
        ],
    };

    println!("{}", render_block(&lines, wrap));
    Ok(())
}

fn write_trace_line(
    path: &PathBuf,
    label: &str,
    _cli: &Cli,
    start_ts: &chrono::DateTime<Utc>,
    duration_ms: u128,
    exit_code: i32,
    error: Option<&CliError>,
) -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let payload = serde_json::json!({
        "start_ts": start_ts.to_rfc3339(),
        "end_ts": (*start_ts
            + chrono::Duration::from_std(Duration::from_millis(duration_ms as u64)).unwrap_or_default())
        .to_rfc3339(),
        "duration_ms": duration_ms,
        "cmd": label,
        "args": args,
        "exit_code": exit_code,
        "error": error.map(|e| serde_json::json!({
            "code": e.code,
            "kind": e.kind,
            "message": e.message,
            "hint": e.hint,
            "retryable": e.retryable,
        })),
        "contract_version": CONTRACT_VERSION,
        "crate_version": env!("CARGO_PKG_VERSION"),
    });

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", payload)?;
    Ok(())
}

/// Time filter helper for search commands
#[derive(Debug, Clone, Default)]
pub struct TimeFilter {
    pub since: Option<i64>,
    pub until: Option<i64>,
}

impl TimeFilter {
    pub fn new(
        days: Option<u32>,
        today: bool,
        yesterday: bool,
        week: bool,
        since_str: Option<&str>,
        until_str: Option<&str>,
    ) -> Self {
        use chrono::{Datelike, Duration, Local, TimeZone};

        let now = Local::now();
        let today_start = Local
            .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
            .unwrap();

        let (since, until) = if today {
            (Some(today_start.timestamp_millis()), None)
        } else if yesterday {
            let yesterday_start = today_start - Duration::days(1);
            (
                Some(yesterday_start.timestamp_millis()),
                Some(today_start.timestamp_millis()),
            )
        } else if week {
            let week_ago = now - Duration::days(7);
            (Some(week_ago.timestamp_millis()), None)
        } else if let Some(d) = days {
            let days_ago = now - Duration::days(d as i64);
            (Some(days_ago.timestamp_millis()), None)
        } else {
            (None, None)
        };

        // Explicit --since/--until override convenience flags when they parse successfully
        let since = since_str.and_then(parse_datetime_str).or(since);
        let until = until_str.and_then(parse_datetime_str).or(until);

        TimeFilter { since, until }
    }
}

fn parse_datetime_str(s: &str) -> Option<i64> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};

    // Try full datetime first: YYYY-MM-DDTHH:MM:SS
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Local
            .from_local_datetime(&dt)
            .single()
            .map(|d| d.timestamp_millis());
    }

    // Try date only: YYYY-MM-DD
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Local
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .single()
            .map(|d| d.timestamp_millis());
    }

    None
}

/// Tool call/result info extracted from source file
#[derive(Debug, Clone)]
struct ToolInfo {
    name: String,
    #[allow(dead_code)]
    id: String,
    input: Option<String>,    // Tool call input (for tool_use)
    output: Option<String>,   // Tool result output (for tool_result)
}

/// Fetch tool calls and results from a source JSONL file around a matched message.
/// Returns tools within the same conversation turn only (stops at turn boundaries).
fn fetch_tools_from_source(source_path: &str, match_line: usize, limit: usize, match_role: Option<&str>) -> Vec<ToolInfo> {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let file = match File::open(source_path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = BufReader::new(file);
    let mut tools = Vec::new();
    let mut tool_calls: HashMap<String, (String, Option<String>)> = HashMap::new(); // id -> (name, input)

    // Define search window based on role:
    //
    // IMPORTANT: We only match on TEXT messages (user text or assistant text).
    // Tool invocations are filtered from search results, and tool_results aren't indexed.
    // See docs/tool-matching.md for detailed explanation.
    //
    // - User TEXT: "Please fix the bug" → tools are FORWARD (Claude's response follows)
    // - Assistant TEXT: "I fixed it" → tools are BACKWARD (Claude used tools before writing text)
    let (window_start, window_end) = match match_role {
        Some("user") => (match_line, match_line + 100), // Forward only for user text
        _ => (match_line.saturating_sub(50), match_line + 10), // Backward for assistant text
    };
    let mut found_turn_boundary = false;

    for (idx, line_result) in reader.lines().enumerate() {
        let line_num = idx + 1; // 1-indexed
        if line_num < window_start {
            continue;
        }
        if line_num > window_end || found_turn_boundary {
            break;
        }

        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };

        let val: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Check for turn boundary (text message that's not tool-related)
        // For user match looking forward: stop at next user text message
        // For assistant match looking backward: we're already bounded by window_end = match_line
        if match_role == Some("user") && line_num > match_line {
            // Check if this is a user TEXT message (not tool_result)
            if msg_type == "user" {
                if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
                    // If content is a string (text), not array (tool_result), it's a turn boundary
                    if content.is_string() {
                        found_turn_boundary = true;
                        continue;
                    }
                }
            }
            // Also check for assistant text message (next response starting)
            if msg_type == "assistant" {
                if let Some(content) = val.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
                    // Check if any content item is text (not tool_use)
                    if content.iter().any(|item| item.get("type").and_then(|t| t.as_str()) == Some("text")) {
                        found_turn_boundary = true;
                        continue;
                    }
                }
            }
        }

        // Handle Claude Code format (tool_use in assistant messages, tool_result in user messages)
        if msg_type == "assistant" {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
                for item in content {
                    if item.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let input = item.get("input").map(|v| {
                            let s = serde_json::to_string(v).unwrap_or_default();
                            if limit > 0 && s.chars().count() > limit {
                                format!("{}...", s.chars().take(limit).collect::<String>())
                            } else {
                                s
                            }
                        });
                        if !id.is_empty() {
                            tool_calls.insert(id.clone(), (name.clone(), input.clone()));
                            tools.push(ToolInfo {
                                name,
                                id,
                                input,
                                output: None,
                            });
                        }
                    }
                }
            }
        } else if msg_type == "user" {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
                for item in content {
                    if item.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                        let tool_use_id = item.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let output_raw = item.get("content").map(|v| {
                            if let Some(s) = v.as_str() {
                                s.to_string()
                            } else {
                                serde_json::to_string(v).unwrap_or_default()
                            }
                        });
                        let output = output_raw.map(|s| {
                            if limit > 0 && s.chars().count() > limit {
                                format!("{}...", s.chars().take(limit).collect::<String>())
                            } else {
                                s
                            }
                        });

                        // Find matching tool call and update with result
                        if tool_calls.contains_key(&tool_use_id) {
                            // Find and update the existing tool entry
                            for tool in &mut tools {
                                if tool.id == tool_use_id && tool.output.is_none() {
                                    tool.output = output.clone();
                                    break;
                                }
                            }
                        } else if !tool_use_id.is_empty() {
                            // Orphan result (call was outside window)
                            tools.push(ToolInfo {
                                name: "?".to_string(),
                                id: tool_use_id,
                                input: None,
                                output,
                            });
                        }
                    }
                }
            }
        }

        // Handle Codex format - check for function_call events
        if msg_type == "response_item" {
            if let Some(payload) = val.get("payload") {
                let item_type = payload.get("type").and_then(|v| v.as_str());
                if item_type == Some("function_call") {
                    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                    let id = payload.get("call_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let input = payload.get("arguments").and_then(|v| v.as_str()).map(|s| {
                        if limit > 0 && s.chars().count() > limit {
                            format!("{}...", s.chars().take(limit).collect::<String>())
                        } else {
                            s.to_string()
                        }
                    });
                    if !id.is_empty() {
                        tool_calls.insert(id.clone(), (name.clone(), input.clone()));
                        tools.push(ToolInfo {
                            name,
                            id,
                            input,
                            output: None,
                        });
                    }
                } else if item_type == Some("function_call_output") {
                    let call_id = payload.get("call_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let output_raw = payload.get("output").and_then(|v| v.as_str()).map(String::from);
                    let output = output_raw.map(|s| {
                        if limit > 0 && s.chars().count() > limit {
                            format!("{}...", s.chars().take(limit).collect::<String>())
                        } else {
                            s
                        }
                    });

                    // Find and update matching tool
                    for tool in &mut tools {
                        if tool.id == call_id && tool.output.is_none() {
                            tool.output = output.clone();
                            break;
                        }
                    }
                }
            }
        }
    }

    tools
}

/// Check if a model matches any of the provided patterns.
/// Patterns support glob-like matching:
/// - "opus" → contains (case-insensitive)
/// - "gpt*" → prefix match
/// - "*opus" → suffix match
/// - "*claude*" → contains (explicit)
/// - "\"exact\"" → exact match (quoted)
fn model_matches(model: &Option<String>, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let Some(model) = model else {
        return false;
    };
    let model_lower = model.to_lowercase();
    patterns.iter().any(|pattern| {
        let trimmed = pattern.trim();
        // Exact match: quoted string
        if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() > 2 {
            let exact = &trimmed[1..trimmed.len() - 1];
            return model_lower == exact.to_lowercase();
        }
        let has_prefix_glob = trimmed.starts_with('*');
        let has_suffix_glob = trimmed.ends_with('*');
        if has_prefix_glob || has_suffix_glob {
            let inner = trimmed.trim_matches('*').to_lowercase();
            match (has_prefix_glob, has_suffix_glob) {
                (true, true) => model_lower.contains(&inner),
                (true, false) => model_lower.ends_with(&inner),
                (false, true) => model_lower.starts_with(&inner),
                _ => unreachable!(),
            }
        } else {
            // Default: contains match
            model_lower.contains(&trimmed.to_lowercase())
        }
    })
}

/// Check if content is a summary/continuation message that should be filtered out.
/// Matches messages like "This session is being continued from a previous conversation".
fn is_summary_message(content: &str) -> bool {
    content.contains("This session is being continued from a previous conversation")
        || content.contains("conversation is summarized below")
        || content.contains("context compaction")
}

/// Check if content is an IDE autocontext message that should be filtered out.
/// Matches messages starting with "# Context from my IDE setup".
fn is_autocontext_message(content: &str) -> bool {
    content.starts_with("# Context from my IDE setup")
        || content.contains("\n# Context from my IDE setup")
}

#[allow(clippy::too_many_arguments)]
fn run_cli_search(
    query: &str,
    agents: &[String],
    workspaces: &[String],
    limit: &usize,
    offset: &usize,
    json: &bool,
    data_dir_override: &Option<PathBuf>,
    db_override: Option<PathBuf>,
    _wrap: WrapConfig,
    _progress: ProgressResolved,
    time_filter: TimeFilter,
    turns: Option<String>,
    snippet_len: usize,
    no_content: bool,
    full_model: bool,
    from: Option<String>,
    model_patterns: Vec<String>,
    tools: Option<usize>,
    include_summaries: bool,
    include_autocontext: bool,
) -> CliResult<()> {
    use crate::search::query::{SearchClient, SearchFilters};
    use crate::search::tantivy::index_dir;
    use std::collections::HashSet;

    // Resolve tools limit: usize::MAX is sentinel for "inherit from snippet_len"
    let tools = tools.map(|t| if t == usize::MAX { snippet_len } else { t });

    let data_dir = data_dir_override.clone().unwrap_or_else(default_data_dir);
    let index_path = index_dir(&data_dir).map_err(|e| CliError {
        code: 9,
        kind: "path",
        message: format!("failed to open index dir: {e}"),
        hint: None,
        retryable: false,
    })?;
    let db_path = db_override.unwrap_or_else(|| data_dir.join("agent_search.db"));

    let client = SearchClient::open(&index_path, Some(&db_path))
        .map_err(|e| CliError {
            code: 9,
            kind: "open-index",
            message: format!("failed to open index: {e}"),
            hint: Some("try cass index --full".to_string()),
            retryable: true,
        })?
        .ok_or_else(|| CliError {
            code: 3,
            kind: "missing-index",
            message: format!(
                "Index not found at {}. Run 'cass index --full' first.",
                index_path.display()
            ),
            hint: None,
            retryable: true,
        })?;

    let mut filters = SearchFilters::default();
    if !agents.is_empty() {
        filters.agents = HashSet::from_iter(agents.iter().cloned());
    }
    if !workspaces.is_empty() {
        filters.workspaces = HashSet::from_iter(workspaces.iter().cloned());
    }
    filters.created_from = time_filter.since;
    filters.created_to = time_filter.until;
    filters.role = from.clone();
    filters.models = model_patterns.clone();

    // Determine if we need post-filtering (which may reduce result count below limit)
    let needs_post_filter = from.is_some()
        || !model_patterns.is_empty()
        || !include_summaries
        || !include_autocontext;

    // Helper closure to apply all post-filters to a hit
    let passes_filters = |hit: &crate::search::query::SearchHit| -> bool {
        // Role filter (--from user/assistant)
        if let Some(ref role_filter) = from {
            if hit.role.as_ref() != Some(role_filter) {
                return false;
            }
        }
        // Model filter
        if !model_patterns.is_empty() {
            if !model_matches(&hit.author, &model_patterns) {
                return false;
            }
        }
        // Summary filter
        if !include_summaries && is_summary_message(&hit.content) {
            return false;
        }
        // Autocontext filter
        if !include_autocontext && is_autocontext_message(&hit.content) {
            return false;
        }
        true
    };

    // Fetch results, looping to get more if post-filtering reduces count below limit.
    // We implement offset via the limit cursor: fetch (offset + limit) filtered results,
    // then skip the first `offset`. This makes offset semantically correct with post-filtering.
    let target_count = *offset + *limit;
    let mut hits = Vec::new();
    let mut raw_offset = 0usize;
    let batch_size = if needs_post_filter { target_count.max(50) } else { target_count };
    let max_iterations = 20; // Safety limit to avoid infinite loops

    for _ in 0..max_iterations {
        let batch = client
            .search(query, filters.clone(), batch_size, raw_offset)
            .map_err(|e| CliError {
                code: 9,
                kind: "search",
                message: format!("search failed: {e}"),
                hint: None,
                retryable: true,
            })?;

        let batch_len = batch.len();
        if batch_len == 0 {
            break; // No more results available
        }

        for mut hit in batch {
            // Enrich Tantivy hits with role/model from SQLite (needed for filtering)
            if hit.role.is_none() || hit.author.is_none() || hit.line_number.is_none() {
                let _ = client.enrich_hit_for_context(&mut hit);
            }

            if passes_filters(&hit) {
                hits.push(hit);
                if hits.len() >= target_count {
                    break;
                }
            }
        }

        if hits.len() >= target_count {
            break; // Got enough results
        }

        raw_offset += batch_len;

        // If batch was smaller than requested, we've exhausted results
        if batch_len < batch_size {
            break;
        }
    }

    // Apply offset: skip first `offset` filtered results, keep up to `limit`
    let mut hits: Vec<_> = hits.into_iter().skip(*offset).take(*limit).collect();

    // Fetch surrounding turns if requested
    // Parse turns as either "N" (symmetric) or "before:after" (asymmetric)
    if let Some(turns_str) = turns {
        let (turns_before, turns_after) = if turns_str.contains(':') {
            let parts: Vec<&str> = turns_str.split(':').collect();
            if parts.len() == 2 {
                let before = parts[0].trim().parse::<usize>().unwrap_or(2);
                let after = parts[1].trim().parse::<usize>().unwrap_or(2);
                (before, after)
            } else {
                (2, 2) // fallback default
            }
        } else {
            let n = turns_str.parse::<usize>().unwrap_or(2);
            (n, n) // symmetric
        };

        for hit in &mut hits {
            // If hit doesn't have conversation_id (e.g., from Tantivy), enrich it from SQLite
            if hit.conversation_id.is_none() {
                let _ = client.enrich_hit_for_context(hit);
            }
            if let (Some(conv_id), Some(line_num)) = (hit.conversation_id, hit.line_number) {
                // line_number is 1-indexed, convert back to 0-indexed idx
                let match_idx = (line_num - 1) as i64;
                if let Ok(context) = client.fetch_surrounding_turns(conv_id, match_idx, turns_before, turns_after) {
                    hit.context = Some(context);
                }
            }
        }
    }

    // Additional enrichment for tools display
    if tools.is_some() {
        for hit in &mut hits {
            if hit.line_number.is_none() {
                let _ = client.enrich_hit_for_context(hit);
            }
        }
    }

    if *json {
        // Apply snippet_len to JSON content if not 0 (full)
        let hits_for_json: Vec<_> = if snippet_len > 0 {
            hits.iter().map(|hit| {
                let mut h = hit.clone();
                if h.content.chars().count() > snippet_len {
                    h.content = h.content.chars().take(snippet_len).collect::<String>() + "...";
                }
                // Also truncate context turn content
                if let Some(ref mut ctx) = h.context {
                    for turn in ctx.iter_mut() {
                        if turn.content.chars().count() > snippet_len {
                            turn.content = turn.content.chars().take(snippet_len).collect::<String>() + "...";
                        }
                    }
                }
                h
            }).collect()
        } else {
            hits.clone()
        };
        let payload = serde_json::json!({
            "query": query,
            "limit": limit,
            "offset": offset,
            "count": hits_for_json.len(),
            "hits": hits_for_json,
            "snippet_len": if snippet_len > 0 { Some(snippet_len) } else { None::<usize> },
        });
        let out = serde_json::to_string_pretty(&payload).map_err(|e| CliError {
            code: 9,
            kind: "encode-json",
            message: format!("failed to encode json: {e}"),
            hint: None,
            retryable: false,
        })?;
        println!("{}", out);
    } else if hits.is_empty() {
        eprintln!("No results found.");
    } else {
        // ANSI color codes
        let (user_color, agent_color, reset, dim, bold) = if std::io::stdout().is_terminal() {
            ("\x1b[34m", "\x1b[32m", "\x1b[0m", "\x1b[2m", "\x1b[1m")
        } else {
            ("", "", "", "", "")
        };

        for (i, hit) in hits.iter().enumerate() {
            // Better separator with hit number
            println!("{}═══════════════════════════════════════════════════════════════════{}", dim, reset);
            // Build headline with optional model (in parentheses after agent, dimmed)
            let model_str = hit.author.as_ref()
                .map(|m| format!(" {}({}){}", dim, m, reset))
                .unwrap_or_default();
            println!("{}[{}/{}]{} Score: {:.2} | {}{} | WS: {}",
                bold, i + 1, hits.len(), reset, hit.score, hit.agent, model_str, hit.workspace
            );
            println!("Path: {}", hit.source_path);

            // Show matched message in context turn format unless --no-content
            if !no_content {
                // Format: >>> [agent] timestamp [N chars]
                //           content...
                // Color based on role (user=blue, assistant=green)
                let role_color = if hit.role.as_deref() == Some("user") { user_color } else { agent_color };
                // Show [User] for user messages, [model] or [agent] for assistant
                let display_name = if hit.role.as_deref() == Some("user") {
                    "User".to_string()
                } else {
                    hit.author.clone().unwrap_or_else(|| hit.agent.clone())
                };
                let agent_display = format!("{}[{}]{}", role_color, display_name, reset);

                // Format timestamp if available
                let ts_display = hit.created_at
                    .and_then(|ts| chrono::DateTime::from_timestamp_millis(ts))
                    .map(|dt| format!(" {}", dt.format("%Y-%m-%d %H:%M:%S")))
                    .unwrap_or_default();

                let char_count = hit.content.chars().count();
                println!("\n>>> {}{}{} [{} chars]", agent_display, ts_display, reset, char_count);

                // Show content with indentation
                if snippet_len == 0 {
                    // Full content with preserved newlines
                    for line in hit.content.lines() {
                        println!("  {}", line);
                    }
                } else {
                    // Truncated preview
                    let effective_len = if snippet_len == 200 { 500 } else { snippet_len }; // Default to longer for this format
                    let preview: String = hit.content.chars().take(effective_len).collect();
                    let ellipsis = if hit.content.chars().count() > effective_len { "..." } else { "" };
                    for line in preview.lines() {
                        println!("  {}", line);
                    }
                    if !ellipsis.is_empty() {
                        println!("  {}", ellipsis);
                    }
                }
            }

            // Print context turns if available
            if let Some(ref context) = hit.context {
                let hint = if snippet_len == 0 { "" } else { ", use --json or -S 0 for full content" };
                println!("\nContext ({} messages{}):", context.len(), hint);
                for turn in context {
                    let marker = if turn.is_match { ">>>" } else { "   " };
                    let role_color = if turn.role == "user" { user_color } else { agent_color };
                    // Show [User] for user messages, [model] or role for assistant
                    let display_name = if turn.role == "user" {
                        "User".to_string()
                    } else {
                        turn.author.clone().unwrap_or_else(|| turn.role.clone())
                    };
                    let role_display = format!("{}[{}]{}", role_color, display_name, reset);

                    // Format timestamp if available
                    let ts_display = turn.created_at
                        .and_then(|ts| chrono::DateTime::from_timestamp_millis(ts))
                        .map(|dt| format!(" {}{}{}", dim, dt.format("%H:%M:%S"), reset))
                        .unwrap_or_default();

                    // Format model if --full-model and available
                    let model_display = if full_model {
                        turn.author.as_ref()
                            .map(|m| format!(" {}[{}]{}", dim, m, reset))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };

                    // Full content: preserve newlines with indent; preview: collapse to single line
                    if snippet_len == 0 {
                        let lines: Vec<&str> = turn.content.lines().collect();
                        if let Some((first, rest)) = lines.split_first() {
                            println!("{} {} {}{}{}: {}", marker, turn.turn_index, role_display, ts_display, model_display, first);
                            for line in rest {
                                println!("       {}", line);
                            }
                        } else {
                            println!("{} {} {}{}{}: ", marker, turn.turn_index, role_display, ts_display, model_display);
                        }
                    } else {
                        let content_clean = turn.content.replace('\n', " ");
                        let preview: String = content_clean.chars().take(snippet_len).collect();
                        let ellipsis = if turn.content.chars().count() > snippet_len { "..." } else { "" };
                        println!(
                            "{} {} {}{}{}: {}{}",
                            marker, turn.turn_index, role_display, ts_display, model_display, preview, ellipsis
                        );
                    }
                }
            }

            // Show tool calls/results if --tools flag is provided
            if let Some(tools_len) = tools {
                if let Some(line_num) = hit.line_number {
                    let fetched_tools = fetch_tools_from_source(&hit.source_path, line_num, tools_len, hit.role.as_deref());
                    // Filter out orphan tool results (those without matching tool_use in window)
                    let useful_tools: Vec<_> = fetched_tools.into_iter().filter(|t| t.name != "?").collect();
                    if !useful_tools.is_empty() {
                        // JSON syntax highlighting colors
                        let key_color = "\x1b[34m";    // Blue for keys
                        let str_color = "\x1b[32m";    // Green for strings
                        let num_color = "\x1b[36m";    // Cyan for numbers
                        let bool_color = "\x1b[33m";   // Yellow for bool/null

                        // Colorize a JSON line (simple regex-free approach)
                        let colorize_json_line = |line: &str| -> String {
                            let mut result = String::new();
                            let mut chars = line.chars().peekable();
                            let mut in_string = false;
                            let mut is_key = false;
                            let mut current_token = String::new();
                            let mut escaped = false;

                            while let Some(c) = chars.next() {
                                if escaped {
                                    current_token.push(c);
                                    escaped = false;
                                    continue;
                                }
                                match c {
                                    '\\' if in_string => {
                                        current_token.push(c);
                                        escaped = true;
                                    }
                                    '"' if !in_string => {
                                        in_string = true;
                                        is_key = result.trim_end().ends_with('{')
                                            || result.trim_end().ends_with(',')
                                            || result.trim_end().is_empty()
                                            || result.ends_with('\n');
                                        current_token.push(c);
                                    }
                                    '"' if in_string => {
                                        current_token.push(c);
                                        let color = if is_key { key_color } else { str_color };
                                        result.push_str(&format!("{}{}{}", color, current_token, reset));
                                        current_token.clear();
                                        in_string = false;
                                    }
                                    _ if in_string => {
                                        current_token.push(c);
                                    }
                                    _ => {
                                        // Check for numbers, booleans, null
                                        if c.is_ascii_digit() || (c == '-' && chars.peek().map(|&n| n.is_ascii_digit()).unwrap_or(false)) {
                                            current_token.push(c);
                                            while let Some(&next) = chars.peek() {
                                                if next.is_ascii_digit() || next == '.' || next == 'e' || next == 'E' || next == '+' || next == '-' {
                                                    current_token.push(chars.next().unwrap());
                                                } else {
                                                    break;
                                                }
                                            }
                                            result.push_str(&format!("{}{}{}", num_color, current_token, reset));
                                            current_token.clear();
                                        } else if c == 't' || c == 'f' || c == 'n' {
                                            current_token.push(c);
                                            while let Some(&next) = chars.peek() {
                                                if next.is_ascii_alphabetic() {
                                                    current_token.push(chars.next().unwrap());
                                                } else {
                                                    break;
                                                }
                                            }
                                            if current_token == "true" || current_token == "false" || current_token == "null" {
                                                result.push_str(&format!("{}{}{}", bool_color, current_token, reset));
                                            } else {
                                                result.push_str(&current_token);
                                            }
                                            current_token.clear();
                                        } else {
                                            result.push(c);
                                        }
                                    }
                                }
                            }
                            result.push_str(&current_token);
                            result
                        };

                        println!("\n{}Tools ({} calls):{}", dim, useful_tools.len(), reset);
                        for tool in &useful_tools {
                            // Tool name header
                            println!("  {}[{}]{}", dim, tool.name, reset);
                            // Input on separate lines (pretty-print JSON with colors)
                            if let Some(ref input) = tool.input {
                                println!("    {}input:{}", dim, reset);
                                // Try to prettify JSON, fall back to raw if parsing fails (e.g., truncated)
                                let prettified = serde_json::from_str::<serde_json::Value>(input)
                                    .ok()
                                    .and_then(|v| serde_json::to_string_pretty(&v).ok());
                                let display_input = prettified.as_ref().unwrap_or(input);
                                for line in display_input.lines() {
                                    println!("      {}", colorize_json_line(line));
                                }
                            }
                            // Output on separate lines (pretty-print JSON if applicable)
                            if let Some(ref output) = tool.output {
                                println!("    {}→ output:{}", dim, reset);
                                // Try to prettify if it looks like JSON
                                let prettified = if output.trim_start().starts_with('{') || output.trim_start().starts_with('[') {
                                    serde_json::from_str::<serde_json::Value>(output)
                                        .ok()
                                        .and_then(|v| serde_json::to_string_pretty(&v).ok())
                                } else {
                                    None
                                };
                                let display_output = prettified.as_ref().unwrap_or(output);
                                for line in display_output.lines() {
                                    // Colorize JSON output too
                                    if prettified.is_some() {
                                        println!("      {}", colorize_json_line(line));
                                    } else {
                                        println!("      {}", line);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        println!("{}═══════════════════════════════════════════════════════════════════{}", dim, reset);
    }

    Ok(())
}

fn run_stats(
    data_dir_override: &Option<PathBuf>,
    db_override: Option<PathBuf>,
    json: bool,
) -> CliResult<()> {
    use rusqlite::Connection;

    let data_dir = data_dir_override.clone().unwrap_or_else(default_data_dir);
    let db_path = db_override.unwrap_or_else(|| data_dir.join("agent_search.db"));

    if !db_path.exists() {
        return Err(CliError {
            code: 3,
            kind: "missing-db",
            message: format!(
                "Database not found at {}. Run 'cass index --full' first.",
                db_path.display()
            ),
            hint: None,
            retryable: true,
        });
    }

    let conn = Connection::open(&db_path).map_err(|e| CliError {
        code: 9,
        kind: "db-open",
        message: format!("Failed to open database: {e}"),
        hint: None,
        retryable: false,
    })?;

    // Get counts and statistics
    let conversation_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap_or(0);
    let message_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap_or(0);

    // Get per-agent breakdown (need to JOIN with agents table)
    let mut agent_stmt = conn
        .prepare(
            "SELECT a.slug, COUNT(*) FROM conversations c JOIN agents a ON c.agent_id = a.id GROUP BY a.slug ORDER BY COUNT(*) DESC"
        )
        .map_err(|e| CliError::unknown(format!("query prep: {e}")))?;
    let agent_rows: Vec<(String, i64)> = agent_stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| CliError::unknown(format!("query: {e}")))?
        .filter_map(|r| r.ok())
        .collect();

    // Get workspace breakdown (top 10, need to JOIN with workspaces table)
    let mut ws_stmt = conn
        .prepare(
            "SELECT w.path, COUNT(*) FROM conversations c JOIN workspaces w ON c.workspace_id = w.id GROUP BY w.path ORDER BY COUNT(*) DESC LIMIT 10"
        )
        .map_err(|e| CliError::unknown(format!("query prep: {e}")))?;
    let ws_rows: Vec<(String, i64)> = ws_stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| CliError::unknown(format!("query: {e}")))?
        .filter_map(|r| r.ok())
        .collect();

    // Get date range
    let oldest: Option<i64> = conn
        .query_row(
            "SELECT MIN(started_at) FROM conversations WHERE started_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .ok();
    let newest: Option<i64> = conn
        .query_row(
            "SELECT MAX(started_at) FROM conversations WHERE started_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .ok();

    if json {
        let payload = serde_json::json!({
            "conversations": conversation_count,
            "messages": message_count,
            "by_agent": agent_rows.iter().map(|(a, c)| serde_json::json!({"agent": a, "count": c})).collect::<Vec<_>>(),
            "top_workspaces": ws_rows.iter().map(|(w, c)| serde_json::json!({"workspace": w, "count": c})).collect::<Vec<_>>(),
            "date_range": {
                "oldest": oldest.map(|ts| chrono::DateTime::from_timestamp_millis(ts).map(|d| d.to_rfc3339())),
                "newest": newest.map(|ts| chrono::DateTime::from_timestamp_millis(ts).map(|d| d.to_rfc3339())),
            },
            "db_path": db_path.display().to_string(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else {
        println!("CASS Index Statistics");
        println!("=====================");
        println!("Database: {}", db_path.display());
        println!();
        println!("Totals:");
        println!("  Conversations: {}", conversation_count);
        println!("  Messages: {}", message_count);
        println!();
        println!("By Agent:");
        for (agent, count) in &agent_rows {
            println!("  {}: {}", agent, count);
        }
        println!();
        if !ws_rows.is_empty() {
            println!("Top Workspaces:");
            for (ws, count) in &ws_rows {
                println!("  {}: {}", ws, count);
            }
            println!();
        }
        if let (Some(old), Some(new)) = (oldest, newest)
            && let (Some(old_dt), Some(new_dt)) = (
                chrono::DateTime::from_timestamp_millis(old),
                chrono::DateTime::from_timestamp_millis(new),
            )
        {
            println!(
                "Date Range: {} to {}",
                old_dt.format("%Y-%m-%d"),
                new_dt.format("%Y-%m-%d")
            );
        }
    }

    Ok(())
}

fn run_view(path: &PathBuf, line: Option<usize>, context: usize, json: bool) -> CliResult<()> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    if !path.exists() {
        return Err(CliError {
            code: 3,
            kind: "file-not-found",
            message: format!("File not found: {}", path.display()),
            hint: None,
            retryable: false,
        });
    }

    let file = File::open(path).map_err(|e| CliError {
        code: 9,
        kind: "file-open",
        message: format!("Failed to open file: {e}"),
        hint: None,
        retryable: false,
    })?;

    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    if lines.is_empty() {
        return Err(CliError {
            code: 9,
            kind: "empty-file",
            message: format!("File is empty: {}", path.display()),
            hint: None,
            retryable: false,
        });
    }

    let target_line = line.unwrap_or(1);

    // Validate target line is within bounds
    if target_line == 0 {
        return Err(CliError {
            code: 2,
            kind: "invalid-line",
            message: "Line numbers start at 1, not 0".to_string(),
            hint: Some("Use -n 1 for the first line".to_string()),
            retryable: false,
        });
    }

    if target_line > lines.len() {
        return Err(CliError {
            code: 2,
            kind: "line-out-of-range",
            message: format!(
                "Line {} exceeds file length ({} lines)",
                target_line,
                lines.len()
            ),
            hint: Some(format!("Use -n {} for the last line", lines.len())),
            retryable: false,
        });
    }

    let start = target_line.saturating_sub(context + 1);
    let end = (target_line + context).min(lines.len());

    // Only highlight a specific line if -n was explicitly provided
    let highlight_line = line.is_some();

    if json {
        let content_lines: Vec<serde_json::Value> = lines
            .iter()
            .enumerate()
            .skip(start)
            .take(end - start)
            .map(|(i, l)| {
                serde_json::json!({
                    "line": i + 1,
                    "content": l,
                    "highlighted": highlight_line && i + 1 == target_line,
                })
            })
            .collect();

        let payload = serde_json::json!({
            "path": path.display().to_string(),
            "target_line": if highlight_line { Some(target_line) } else { None::<usize> },
            "context": context,
            "lines": content_lines,
            "total_lines": lines.len(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else {
        println!("File: {}", path.display());
        if highlight_line {
            println!("Line: {} (context: {})", target_line, context);
        }
        println!("----------------------------------------");
        for (i, l) in lines.iter().enumerate().skip(start).take(end - start) {
            let line_num = i + 1;
            let marker = if highlight_line && line_num == target_line {
                ">"
            } else {
                " "
            };
            println!("{}{:5} | {}", marker, line_num, l);
        }
        println!("----------------------------------------");
        if lines.len() > end {
            println!("... ({} more lines)", lines.len() - end);
        }
    }

    Ok(())
}

fn spawn_background_indexer(
    data_dir: PathBuf,
    db: Option<PathBuf>,
    progress: Option<std::sync::Arc<indexer::IndexingProgress>>,
) {
    std::thread::spawn(move || {
        let db_path = db.unwrap_or_else(|| data_dir.join("agent_search.db"));
        let opts = IndexOptions {
            full: false,
            force_rebuild: false,
            watch: true,
            db_path,
            data_dir,
            progress,
        };
        if let Err(e) = indexer::run_index(opts) {
            warn!("Background indexer failed: {}", e);
        }
    });
}

fn run_index_with_data(
    db_override: Option<PathBuf>,
    full: bool,
    force_rebuild: bool,
    watch: bool,
    data_dir_override: Option<PathBuf>,
    progress: ProgressResolved,
) -> CliResult<()> {
    let data_dir = data_dir_override.unwrap_or_else(default_data_dir);
    let db_path = db_override.unwrap_or_else(|| data_dir.join("agent_search.db"));
    let opts = IndexOptions {
        full,
        force_rebuild,
        watch,
        db_path,
        data_dir,
        progress: None,
    };
    let spinner = match progress {
        ProgressResolved::Bars => Some(indicatif::ProgressBar::new_spinner()),
        ProgressResolved::Plain => None,
        ProgressResolved::None => None,
    };
    if let Some(pb) = &spinner {
        pb.set_message(if full { "index --full" } else { "index" });
        pb.enable_steady_tick(Duration::from_millis(120));
    } else if matches!(progress, ProgressResolved::Plain) {
        eprintln!("index starting (full={}, watch={})", full, watch);
    }

    let res = indexer::run_index(opts).map_err(|e| {
        let chain = e
            .chain()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        CliError {
            code: 9,
            kind: "index",
            message: format!("index failed: {chain}"),
            hint: None,
            retryable: true,
        }
    });

    if let Err(err) = &res {
        eprintln!("index debug error: {err:?}");
    }

    if let Some(pb) = spinner {
        pb.finish_and_clear();
    } else if matches!(progress, ProgressResolved::Plain) {
        eprintln!("index completed");
    }

    res
}

pub fn default_db_path() -> PathBuf {
    default_data_dir().join("agent_search.db")
}

pub fn default_data_dir() -> PathBuf {
    directories::ProjectDirs::from("com", "coding-agent-search", "coding-agent-search")
        .expect("project dirs available")
        .data_dir()
        .to_path_buf()
}

const OWNER: &str = "Dicklesworthstone";
const REPO: &str = "coding_agent_session_search";

#[derive(Debug, Deserialize)]
struct ReleaseInfo {
    tag_name: String,
}

async fn maybe_prompt_for_update(once: bool) -> Result<()> {
    if once
        || std::env::var("CI").is_ok()
        || std::env::var("TUI_HEADLESS").is_ok()
        || std::env::var("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT").is_ok()
        || !io::stdin().is_terminal()
    {
        return Ok(());
    }

    let client = Client::builder()
        .user_agent("coding-agent-search (update-check)")
        .timeout(Duration::from_secs(3))
        .build()?;

    let Some((latest_tag, latest_ver)) = latest_release_version(&client).await else {
        return Ok(());
    };

    let current_ver =
        Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 1, 0));
    if latest_ver <= current_ver {
        return Ok(());
    }

    println!(
        "A newer version is available: current v{}, latest {}. Update now? (y/N): ",
        current_ver, latest_tag
    );
    print!("> ");
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return Ok(());
    }
    if !matches!(input.trim(), "y" | "Y") {
        return Ok(());
    }

    info!(target: "update", "starting self-update to {}", latest_tag);
    match run_self_update(&latest_tag) {
        Ok(true) => {
            println!("Update complete. Please restart cass.");
            std::process::exit(0);
        }
        Ok(false) => {
            warn!(target: "update", "self-update failed (installer returned error)");
        }
        Err(err) => {
            warn!(target: "update", "self-update failed: {err}");
        }
    }

    Ok(())
}

async fn latest_release_version(client: &Client) -> Option<(String, Version)> {
    let url = format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest");
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let info: ReleaseInfo = resp.json().await.ok()?;
    let tag = info.tag_name;
    let version_str = tag.trim_start_matches('v');
    let version = Version::parse(version_str).ok()?;
    Some((tag, version))
}

#[cfg(windows)]
fn run_self_update(tag: &str) -> Result<bool> {
    let ps_cmd = format!(
        "irm https://raw.githubusercontent.com/{OWNER}/{REPO}/{tag}/install.ps1 | iex; install.ps1 -EasyMode -Verify -Version {tag}"
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps_cmd])
        .status()?;
    if status.success() {
        info!(target: "update", "updated to {tag}");
        Ok(true)
    } else {
        warn!(target: "update", "installer returned non-zero status: {status:?}");
        Ok(false)
    }
}

#[cfg(not(windows))]
fn run_self_update(tag: &str) -> Result<bool> {
    let sh_cmd = format!(
        "curl -fsSL https://raw.githubusercontent.com/{OWNER}/{REPO}/{tag}/install.sh | bash -s -- --easy-mode --verify --version {tag}"
    );
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&sh_cmd)
        .status()?;
    if status.success() {
        info!(target: "update", "updated to {tag}");
        Ok(true)
    } else {
        warn!(target: "update", "installer returned non-zero status: {status:?}");
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_matches_contains_pattern() {
        // Default pattern without globs is a case-insensitive contains match
        assert!(model_matches(&Some("claude-sonnet-4-20250514".to_string()), &["sonnet".to_string()]));
        assert!(model_matches(&Some("claude-sonnet-4-20250514".to_string()), &["SONNET".to_string()]));
        assert!(model_matches(&Some("gpt-4o-mini".to_string()), &["4o".to_string()]));
        assert!(!model_matches(&Some("claude-opus-4".to_string()), &["sonnet".to_string()]));
    }

    #[test]
    fn model_matches_prefix_glob() {
        // Pattern ending with * is a prefix match
        assert!(model_matches(&Some("gpt-4o-mini".to_string()), &["gpt*".to_string()]));
        assert!(model_matches(&Some("gpt-4".to_string()), &["gpt*".to_string()]));
        assert!(!model_matches(&Some("claude-opus".to_string()), &["gpt*".to_string()]));
    }

    #[test]
    fn model_matches_suffix_glob() {
        // Pattern starting with * is a suffix match
        assert!(model_matches(&Some("claude-opus-4".to_string()), &["*opus-4".to_string()]));
        assert!(model_matches(&Some("claude-sonnet-4-20250514".to_string()), &["*20250514".to_string()]));
        assert!(!model_matches(&Some("gpt-4o".to_string()), &["*opus".to_string()]));
    }

    #[test]
    fn model_matches_both_globs() {
        // Pattern with * on both sides is a contains match (explicit)
        assert!(model_matches(&Some("claude-sonnet-4-20250514".to_string()), &["*sonnet*".to_string()]));
        assert!(model_matches(&Some("claude-opus-4".to_string()), &["*opus*".to_string()]));
    }

    #[test]
    fn model_matches_exact_quoted() {
        // Quoted pattern is an exact case-insensitive match
        assert!(model_matches(&Some("claude-opus-4".to_string()), &["\"claude-opus-4\"".to_string()]));
        assert!(model_matches(&Some("Claude-Opus-4".to_string()), &["\"claude-opus-4\"".to_string()]));
        assert!(!model_matches(&Some("claude-opus-4-beta".to_string()), &["\"claude-opus-4\"".to_string()]));
    }

    #[test]
    fn model_matches_empty_patterns() {
        // Empty patterns list matches everything
        assert!(model_matches(&Some("anything".to_string()), &[]));
        assert!(model_matches(&None, &[]));
    }

    #[test]
    fn model_matches_none_model() {
        // None model doesn't match any pattern (except empty list)
        assert!(!model_matches(&None, &["opus".to_string()]));
        assert!(!model_matches(&None, &["*".to_string()]));
    }

    #[test]
    fn model_matches_multiple_patterns() {
        // Any pattern match is sufficient (OR logic)
        assert!(model_matches(&Some("claude-opus-4".to_string()), &["sonnet".to_string(), "opus".to_string()]));
        assert!(model_matches(&Some("gpt-4o".to_string()), &["claude*".to_string(), "gpt*".to_string()]));
        assert!(!model_matches(&Some("gemini-pro".to_string()), &["claude*".to_string(), "gpt*".to_string()]));
    }

    #[test]
    fn fetch_tools_extracts_tool_use_from_assistant_message() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.jsonl");

        // Create JSONL with tool_use in assistant message
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"content":"Help me read a file"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_123","name":"Read","input":{{"file_path":"/test.txt"}}}}]}}}}"#).unwrap();
        drop(file);

        let tools = fetch_tools_from_source(file_path.to_str().unwrap(), 2, 0, Some("assistant"));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "Read");
        assert_eq!(tools[0].id, "toolu_123");
        assert!(tools[0].input.is_some());
    }

    #[test]
    fn fetch_tools_extracts_tool_result_from_user_message() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.jsonl");

        // Create JSONL with tool_use followed by tool_result
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"content":"Help me"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_abc","name":"Bash","input":{{"command":"ls"}}}}]}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"toolu_abc","content":"file1.txt\nfile2.txt"}}]}}}}"#).unwrap();
        drop(file);

        // Looking forward from user message at line 1
        let tools = fetch_tools_from_source(file_path.to_str().unwrap(), 1, 0, Some("user"));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "Bash");
        assert!(tools[0].output.is_some());
        assert!(tools[0].output.as_ref().unwrap().contains("file1.txt"));
    }

    #[test]
    fn fetch_tools_respects_limit() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.jsonl");

        // Create JSONL with long input
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"content":"Help"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_x","name":"Write","input":{{"content":"This is a very long content string that should be truncated when limit is applied"}}}}]}}}}"#).unwrap();
        drop(file);

        let tools = fetch_tools_from_source(file_path.to_str().unwrap(), 2, 20, Some("assistant"));
        assert_eq!(tools.len(), 1);
        // With limit=20, input should be truncated
        let input = tools[0].input.as_ref().unwrap();
        assert!(input.len() < 100, "Input should be truncated: {}", input);
        assert!(input.ends_with("..."), "Truncated input should end with ...");
    }

    #[test]
    fn fetch_tools_stops_at_turn_boundary() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.jsonl");

        // Create JSONL with tools from different turns
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"content":"First question"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_1","name":"Read","input":{{}}}}]}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"toolu_1","content":"result1"}}]}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Here's the answer"}}]}}}}"#).unwrap();
        // Turn boundary - next user text message
        writeln!(file, r#"{{"type":"user","message":{{"content":"Second question"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_2","name":"Write","input":{{}}}}]}}}}"#).unwrap();
        drop(file);

        // Looking forward from first user message, should only find toolu_1
        let tools = fetch_tools_from_source(file_path.to_str().unwrap(), 1, 0, Some("user"));
        assert_eq!(tools.len(), 1, "Should only find 1 tool (toolu_1), found: {:?}", tools);
        assert_eq!(tools[0].name, "Read");
    }

    #[test]
    fn fetch_tools_handles_missing_file() {
        let tools = fetch_tools_from_source("/nonexistent/path.jsonl", 1, 0, None);
        assert!(tools.is_empty(), "Should return empty vec for missing file");
    }

    #[test]
    fn is_summary_message_detects_continuation() {
        // Matches context continuation messages
        assert!(is_summary_message("This session is being continued from a previous conversation that ran out of context."));
        assert!(is_summary_message("The conversation is summarized below:\n\nPrevious work..."));
        assert!(is_summary_message("Due to context compaction, some history was removed."));
        // Does not match normal content
        assert!(!is_summary_message("Help me write a function"));
        assert!(!is_summary_message("The user's previous session was productive"));
    }

    #[test]
    fn is_autocontext_message_detects_ide_context() {
        // Matches IDE autocontext headers
        assert!(is_autocontext_message("# Context from my IDE setup\nVim settings..."));
        assert!(is_autocontext_message("Some preamble\n# Context from my IDE setup\nSettings"));
        // Does not match normal content with "Context"
        assert!(!is_autocontext_message("Here is some context for the task"));
        assert!(!is_autocontext_message("# IDE Setup Guide\n..."));
    }
}
