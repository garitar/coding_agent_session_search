#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Reset SIGPIPE to default behavior to avoid panic on broken pipe (e.g., when piping to `less`)
    #[cfg(unix)]
    {
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }

    // Load .env early; ignore if missing.
    dotenvy::dotenv().ok();

    match coding_agent_search::run().await {
        Ok(()) => Ok(()),
        Err(err) => {
            let payload = serde_json::json!({
                "error": {
                    "code": err.code,
                    "kind": err.kind,
                    "message": err.message,
                    "hint": err.hint,
                    "retryable": err.retryable,
                }
            });
            eprintln!("{}", payload);
            std::process::exit(err.code);
        }
    }
}
