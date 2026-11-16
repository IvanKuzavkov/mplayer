use crate::*;
use std::{
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot, watch};

pub const INBOX_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TraceId {
    run: u128,
    sequence: u64,
}
impl std::fmt::Display for TraceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:x}-{:x}-{}",
            self.run,
            std::process::id(),
            self.sequence
        )
    }
}

#[derive(Clone, Debug)]
pub struct TraceContext {
    pub id: TraceId,
    pub span: tracing::Span,
}
impl PartialEq for TraceContext {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl TraceContext {
    pub fn new(command: &impl std::fmt::Debug) -> Self {
        static RUN: OnceLock<u128> = OnceLock::new();
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = TraceId {
            run: *RUN.get_or_init(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            }),
            sequence: NEXT.fetch_add(1, Ordering::Relaxed),
        };
        let span =
            tracing::debug_span!(target: "mplayer::commands", "command", trace_id = %id, ?command);
        let trace = Self { id, span };
        trace.event("router", "submitted");
        trace
    }
    pub fn event(&self, component: &str, stage: &str) {
        self.span.in_scope(
            || tracing::debug!(target: "mplayer::commands", trace_id = %self.id, component, stage),
        );
    }
    pub fn failed(&self, component: &str, error: &impl std::fmt::Display) {
        self.span.in_scope(|| tracing::debug!(target: "mplayer::commands", trace_id = %self.id, component, stage = "failed", %error));
    }
    pub fn superseded_by(&self, next: &Self, component: &str) {
        self.span.in_scope(|| tracing::debug!(target: "mplayer::commands", trace_id = %self.id, component, stage = "superseded", replacement_trace_id = %next.id));
    }
    pub fn wrap<T>(&self, payload: T) -> CommandEnvelope<T> {
        CommandEnvelope {
            payload,
            trace: self.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommandEnvelope<T> {
    pub payload: T,
    pub trace: TraceContext,
}
impl<T> CommandEnvelope<T> {
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> CommandEnvelope<U> {
        CommandEnvelope {
            payload: map(self.payload),
            trace: self.trace,
        }
    }
}

#[derive(Clone, Debug)]
pub enum TrackSource {
    Album(AlbumKey),
    Playlist(i64),
}

#[derive(Debug)]
pub enum LibraryRequest {
    Command(LibraryCommand),
    Resolve {
        source: TrackSource,
        reply: oneshot::Sender<Result<Vec<Track>>>,
    },
}

#[derive(Clone)]
pub struct LibraryClient(pub mpsc::Sender<CommandEnvelope<LibraryRequest>>);
impl LibraryClient {
    pub async fn tracks(&self, source: TrackSource, trace: &TraceContext) -> Result<Vec<Track>> {
        let (reply, result) = oneshot::channel();
        self.0
            .send(trace.wrap(LibraryRequest::Resolve { source, reply }))
            .await
            .map_err(|_| anyhow::anyhow!("Library service has stopped"))?;
        result
            .await
            .map_err(|_| anyhow::anyhow!("Library lookup was cancelled"))?
    }
}

#[derive(Clone)]
pub struct CommandRouter {
    playback: mpsc::Sender<CommandEnvelope<PlaybackCommand>>,
    library: LibraryClient,
    shutdown: watch::Sender<bool>,
}
impl CommandRouter {
    pub fn new(
        playback: mpsc::Sender<CommandEnvelope<PlaybackCommand>>,
        library: LibraryClient,
        shutdown: watch::Sender<bool>,
    ) -> Self {
        Self {
            playback,
            library,
            shutdown,
        }
    }
    pub fn prepare(&self, command: impl Into<AppCommand>) -> CommandEnvelope<AppCommand> {
        let payload = command.into();
        let trace = TraceContext::new(&payload);
        trace.wrap(payload)
    }
    pub fn playback_closed(&self) -> bool {
        self.playback.is_closed() || *self.shutdown.borrow()
    }
    pub fn library_closed(&self) -> bool {
        self.library.0.is_closed() || *self.shutdown.borrow()
    }
    pub fn shutdown(&self) {
        let _ = self.try_send(self.prepare(AppCommand::Shutdown));
    }
    #[allow(
        clippy::result_large_err,
        reason = "return the original bounded command inline so retries need no allocation"
    )]
    pub fn try_send(
        &self,
        command: CommandEnvelope<AppCommand>,
    ) -> std::result::Result<(), mpsc::error::TrySendError<CommandEnvelope<AppCommand>>> {
        use mpsc::error::TrySendError::{Closed, Full};
        let CommandEnvelope { payload, trace } = command;
        if *self.shutdown.borrow() {
            trace.event("router", "rejected_closed");
            return Err(Closed(trace.wrap(payload)));
        }
        let result = match payload {
            AppCommand::Playback(command) => {
                self.playback
                    .try_send(trace.wrap(command))
                    .map_err(|error| match error {
                        Full(command) => Full(command.map(AppCommand::Playback)),
                        Closed(command) => Closed(command.map(AppCommand::Playback)),
                    })
            }
            AppCommand::Library(command) => self
                .library
                .0
                .try_send(trace.wrap(LibraryRequest::Command(command)))
                .map_err(|error| {
                    let full = matches!(&error, Full(_));
                    let command = error.into_inner().map(|request| {
                        let LibraryRequest::Command(command) = request else {
                            unreachable!()
                        };
                        AppCommand::Library(command)
                    });
                    if full { Full(command) } else { Closed(command) }
                }),
            AppCommand::Shutdown => {
                self.shutdown.send_replace(true);
                Ok(())
            }
        };
        trace.event(
            "router",
            match &result {
                Ok(()) => "dispatched",
                Err(Full(_)) => "inbox_full",
                Err(Closed(_)) => "rejected_closed",
            },
        );
        result
    }
}

pub struct ArtworkRequest {
    pub path: PathBuf,
    pub reply: oneshot::Sender<Result<Option<Artwork>>>,
}
#[derive(Clone)]
pub struct ArtworkClient(pub mpsc::Sender<CommandEnvelope<ArtworkRequest>>);
impl ArtworkClient {
    pub async fn read(&self, path: PathBuf) -> Result<Option<Artwork>> {
        let trace = TraceContext::new(&("thumbnail", &path));
        let (reply, result) = oneshot::channel();
        self.0
            .send(trace.wrap(ArtworkRequest { path, reply }))
            .await
            .map_err(|_| anyhow::anyhow!("Artwork service has stopped"))?;
        result
            .await
            .map_err(|_| anyhow::anyhow!("Artwork read was cancelled"))?
    }
}

pub fn publish_changed<T: PartialEq + Clone>(sender: &watch::Sender<T>, state: &T) {
    sender.send_if_modified(|previous| {
        if previous == state {
            false
        } else {
            previous.clone_from(state);
            true
        }
    });
}

pub async fn shutdown_requested(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow_and_update() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/messaging.rs"]
mod tests;
