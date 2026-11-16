use mplayer_core::*;
use std::{
    sync::{Arc, mpsc::SyncSender},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};
use tracing::Instrument;
use tracing_appender::non_blocking::ErrorCounter;

#[path = "artwork_service.rs"]
mod artwork;
#[path = "library_service.rs"]
mod library;
#[path = "playback_service.rs"]
mod playback;
#[path = "settings_service.rs"]
mod settings;

pub struct Application {
    pub commands: CommandRouter,
    pub subscriptions: UiSubscriptions,
    pub artwork: ArtworkClient,
    shutdown: watch::Receiver<bool>,
    tasks: JoinSet<(&'static str, Result<()>)>,
    dropped_logs: ErrorCounter,
}
impl Application {
    pub async fn start(
        store: Store,
        audio: SyncSender<CommandEnvelope<AudioCommand>>,
        playback: watch::Receiver<PlaybackSnapshot>,
        probe: Arc<dyn MetadataProbe>,
        dropped_logs: ErrorCounter,
    ) -> Result<Self> {
        let restored = settings::restore(&store).await?;
        let trace = TraceContext::new(&"restore_settings");
        let (desired, settings_updates) = watch::channel(SettingsUpdate {
            volume: trace.wrap(restored.volume),
            device: trace.wrap(restored.device.clone()),
        });
        let (settings_output, settings_state) = watch::channel(restored);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (player_commands, player_inbox) = mpsc::channel(INBOX_CAPACITY);
        let (library_commands, library_inbox) = mpsc::channel(INBOX_CAPACITY);
        let library_client = LibraryClient(library_commands);
        let (artwork_commands, artwork_inbox) = mpsc::channel(INBOX_CAPACITY);
        let (player_output, player_state) = watch::channel(PlayerSnapshot::default());
        let (library_output, library_state) = watch::channel(LibrarySnapshot::default());
        let (artwork_output, artwork_state) = watch::channel(ArtworkSnapshot::default());
        let player = playback::PlaybackService::new(
            audio,
            playback,
            player_inbox,
            player_output,
            library_client.clone(),
            desired,
        );
        let library = library::LibraryService::new(
            store.clone(),
            probe.clone(),
            library_inbox,
            library_output,
        );
        let mut tasks = JoinSet::new();
        let stop = shutdown_rx.clone();
        tasks.spawn(async move { ("playback", player.run(stop).await) }.in_current_span());
        let stop = shutdown_rx.clone();
        tasks.spawn(async move { ("library", library.run(stop).await) }.in_current_span());
        let stop = shutdown_rx.clone();
        let artwork_player = player_state.clone();
        tasks.spawn(
            async move {
                (
                    "artwork",
                    artwork::run(probe, artwork_player, artwork_inbox, artwork_output, stop).await,
                )
            }
            .in_current_span(),
        );
        tasks.spawn(
            async move {
                (
                    "settings",
                    settings::run(store, settings_updates, settings_output).await,
                )
            }
            .in_current_span(),
        );
        Ok(Self {
            commands: CommandRouter::new(player_commands, library_client, shutdown),
            subscriptions: UiSubscriptions {
                player: player_state,
                library: library_state,
                artwork: artwork_state,
                settings: settings_state,
            },
            artwork: ArtworkClient(artwork_commands),
            shutdown: shutdown_rx,
            tasks,
            dropped_logs,
        })
    }
    pub async fn run(mut self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_dropped = 0;
        let mut failure = None;
        loop {
            tokio::select! {
                _ = shutdown_requested(&mut self.shutdown) => break,
                result = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    match result.unwrap() {
                        Ok((service, Err(error))) => { tracing::error!(service, %error, "service stopped"); failure = Some(error); }
                        Err(error) => { tracing::error!(%error, "service task failed"); failure = Some(error.into()); }
                        Ok((service, Ok(()))) => tracing::debug!(service, "service stopped"),
                    }
                }
                _ = tick.tick() => {
                    let dropped = self.dropped_logs.dropped_lines();
                    if dropped != last_dropped { tracing::warn!(dropped, "log buffer overflow; some records were dropped"); last_dropped = dropped; }
                }
            }
        }
        let joined = tokio::time::timeout(Duration::from_secs(4), async {
            while let Some(result) = self.tasks.join_next().await {
                match result {
                    Ok((service, Err(error))) => {
                        tracing::warn!(service, %error, "service shutdown failed");
                        failure = Some(error);
                    }
                    Err(error) => {
                        failure = Some(error.into());
                    }
                    _ => (),
                }
            }
        })
        .await;
        if joined.is_err() {
            self.tasks.abort_all();
            failure = Some(anyhow::anyhow!("Service shutdown timed out"));
        }
        tracing::info!("application services stopped");
        failure.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
#[path = "../tests/unit/app.rs"]
mod tests;
