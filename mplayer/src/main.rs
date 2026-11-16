mod app;
mod config;
mod logging;

#[cfg(test)]
#[path = "../tests/unit/icon.rs"]
mod icon_tests;

use mplayer_audio::{AudioEngine, metadata_probe};
use mplayer_core::{LibraryCommand, Result, Store};
use mplayer_ui::eframe;
use std::{path::PathBuf, sync::Arc};
use tracing::Instrument;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let data = config::data_directory()?;
    tokio::fs::create_dir_all(&data).await?;
    let logs = logging::init(&data.join("logs")).await?;
    let result = run(data, logs.dropped.clone()).await;
    tokio::task::spawn_blocking(move || drop(logs)).await?;
    result
}

#[tracing::instrument(name = "mplayer::run", skip_all, fields(version = env!("CARGO_PKG_VERSION")), err)]
async fn run(
    data: PathBuf,
    dropped_logs: tracing_appender::non_blocking::ErrorCounter,
) -> Result<()> {
    tracing::info!(data_directory = %data.display(), "starting music player");
    let store = Store::open(&data.join("library.sqlite3")).await?;
    let audio = AudioEngine::start()?;
    let app = app::Application::start(
        store.clone(),
        audio.sender(),
        audio.subscribe(),
        Arc::new(metadata_probe()),
        dropped_logs,
    )
    .await?;
    let commands = app.commands.clone();
    let snapshots = app.subscriptions.clone();
    let artwork = app.artwork.clone();
    let task = tokio::spawn(app.run().in_current_span());
    let startup_files: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if !startup_files.is_empty() {
        let _ = commands.try_send(commands.prepare(LibraryCommand::Import(startup_files)));
    }
    let ui_commands = commands.clone();
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/app-icon.png"))?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_icon(icon)
            .with_inner_size([1150.0, 760.0])
            .with_min_inner_size([950.0, 620.0]),
        ..Default::default()
    };
    // Native window event loops must run on the original main thread. Tokio's
    // multithread runtime continues background services on its worker threads.
    let ui_result = eframe::run_native(
        "mplayer",
        options,
        Box::new(move |_creation| {
            Ok(Box::new(
                mplayer_ui::PlayerApp::new(ui_commands, snapshots).with_artwork_client(artwork),
            ))
        }),
    );
    let service_result: Result<()> = async {
        commands.shutdown();
        task.await?
    }
    .await;
    let shutdown_span = tracing::Span::current();
    let audio_result =
        tokio::task::spawn_blocking(move || shutdown_span.in_scope(|| audio.shutdown())).await;
    store.close().await;
    tracing::info!("music player stopped");
    service_result?;
    audio_result??;
    ui_result.map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(())
}
