mod app;
mod config;
mod logging;

#[cfg(test)]
#[path = "../tests/unit/icon.rs"]
mod icon_tests;

use mplayer_audio::{AudioEngine, SymphoniaProbe};
use mplayer_core::{AppCommand, AppSnapshot, Result, Store, Wake};
use mplayer_ui::eframe;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tokio::sync::{mpsc, watch};
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
    let context = Arc::new(OnceLock::<eframe::egui::Context>::new());
    let wake: Wake = {
        let context = Arc::clone(&context);
        Arc::new(move || {
            if let Some(context) = context.get() {
                context.request_repaint();
            }
        })
    };
    let audio = AudioEngine::start(Arc::clone(&wake))?;
    let (commands, receiver) = mpsc::channel(64);
    let (state, snapshots) = watch::channel(AppSnapshot::default());
    let app = app::Application::new(
        store.clone(),
        audio.sender(),
        audio.subscribe(),
        receiver,
        state,
        wake,
        Arc::new(SymphoniaProbe),
        dropped_logs,
    );
    let task = tokio::spawn(app.run().in_current_span());
    let startup_files: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if !startup_files.is_empty() {
        let _ = commands.try_send(AppCommand::Import(startup_files));
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
        Box::new(move |creation| {
            let _ = context.set(creation.egui_ctx.clone());
            Ok(Box::new(
                mplayer_ui::PlayerApp::new(ui_commands, snapshots)
                    .with_artwork_probe(Arc::new(SymphoniaProbe)),
            ))
        }),
    );
    let service_result: Result<()> = async {
        let _ = commands.send(AppCommand::Shutdown).await;
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
