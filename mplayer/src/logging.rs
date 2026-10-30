use std::path::Path;

use mplayer_core::Result;
use tracing_appender::{
    non_blocking::{ErrorCounter, NonBlockingBuilder, WorkerGuard},
    rolling::{Builder, Rotation},
};
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan};

const DEFAULT_FILTER: &str =
    "warn,mplayer=info,mplayer_core=info,mplayer_audio=info,mplayer_ui=info";

pub struct Logging {
    // Drop this only after all application services have stopped.
    _guard: WorkerGuard,
    pub dropped: ErrorCounter,
}

fn rolling_writer(directory: &Path) -> Result<tracing_appender::rolling::RollingFileAppender> {
    Ok(Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("mplayer")
        .filename_suffix("jsonl")
        .max_log_files(2)
        .build(directory)?)
}

pub async fn init(directory: &Path) -> Result<Logging> {
    let directory = directory.to_owned();
    let ready = tokio::fs::create_dir_all(&directory).await;
    // tracing-appender opens/rotates files through std::io::Write. Keep that
    // synchronous setup off the async executor, as with its logging worker.
    tokio::task::spawn_blocking(move || init_writer(&directory, ready)).await?
}

fn init_writer(directory: &Path, ready: std::io::Result<()>) -> Result<Logging> {
    let writer: Box<dyn std::io::Write + Send> = match ready
        .map_err(Into::into)
        .and_then(|()| rolling_writer(directory))
    {
        Ok(writer) => Box::new(writer),
        Err(error) => {
            eprintln!(
                "Cannot open log directory {}: {error}; logging to stderr",
                directory.display()
            );
            Box::new(std::io::stderr())
        }
    };
    let (writer, guard) = NonBlockingBuilder::default()
        .buffered_lines_limit(8192)
        .lossy(true)
        .thread_name("mplayer-logs")
        .finish(writer);
    let dropped = writer.error_counter();
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(error) => {
            if std::env::var_os("RUST_LOG").is_some() {
                eprintln!("Invalid RUST_LOG ({error}); using default logging filter");
            }
            EnvFilter::new(DEFAULT_FILTER)
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_ansi(false)
        .with_current_span(true)
        .with_span_list(true)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(writer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("Failed to create subscriber={e:?}"))?;
    Ok(Logging {
        _guard: guard,
        dropped,
    })
}

#[cfg(test)]
#[path = "../tests/unit/logging.rs"]
mod tests;
