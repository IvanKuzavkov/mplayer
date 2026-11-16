use mplayer_core::*;
use std::{path::PathBuf, sync::Arc};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
};

type CoverRead = (
    PathBuf,
    u64,
    TraceContext,
    JoinHandle<Result<Option<Artwork>>>,
);

pub(super) async fn run(
    probe: Arc<dyn MetadataProbe>,
    mut player: watch::Receiver<PlayerSnapshot>,
    mut requests: mpsc::Receiver<CommandEnvelope<ArtworkRequest>>,
    output: watch::Sender<ArtworkSnapshot>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let mut state = ArtworkSnapshot::default();
    let mut loaded = None;
    let mut reading: Option<CoverRead> = None;
    let mut thumbnails = JoinSet::new();
    let mut requests_closed = false;
    let mut player_closed = false;
    loop {
        let current = player.borrow_and_update().clone();
        let path = current
            .playback
            .track
            .as_ref()
            .map(|track| track.path.clone());
        if state.revision != current.artwork_revision {
            state.revision = current.artwork_revision;
            state.artwork = None;
            loaded = None;
            publish_changed(&output, &state);
        }
        if state.path != path {
            state.path = path;
            loaded = None;
            if state.path.is_none() {
                state.artwork = None;
            }
            publish_changed(&output, &state);
        }
        if reading.is_none()
            && loaded != state.path
            && let Some(path) = state.path.clone()
        {
            let probe = probe.clone();
            let trace = current
                .origin
                .unwrap_or_else(|| TraceContext::new(&("cover", &path)));
            let worker_path = path.clone();
            let worker_trace = trace.clone();
            let task = tokio::task::spawn_blocking(move || {
                worker_trace.span.in_scope(|| probe.artwork(&worker_path))
            });
            reading = Some((path, state.revision, trace, task));
        }
        tokio::select! {
            biased;
            _ = shutdown_requested(&mut shutdown) => break,
            changed = player.changed(), if !player_closed => { player_closed = changed.is_err(); }
            result = async { match reading.as_mut() { Some((_, _, _, task)) => Some(task.await), None => std::future::pending().await } } => {
                let (path, revision, trace, _) = reading.take().unwrap();
                if state.path.as_ref() == Some(&path) && state.revision == revision {
                    loaded = Some(path);
                    state.artwork = match result.unwrap().map_err(anyhow::Error::from).and_then(|result| result) {
                        Ok(artwork) => { trace.event("artwork", "read"); artwork }
                        Err(error) => { trace.failed("artwork", &error); tracing::warn!(%error, "could not read album artwork"); None }
                    };
                } else {
                    trace.event("artwork", "discarded_stale");
                }
                publish_changed(&output, &state);
            }
            request = requests.recv(), if !requests_closed && thumbnails.len() < 2 => {
                if let Some(request) = request {
                    let probe = probe.clone();
                    thumbnails.spawn_blocking(move || {
                        let CommandEnvelope { payload, trace } = request;
                        if payload.reply.is_closed() { trace.event("artwork", "cancelled"); return; }
                        let result = trace.span.in_scope(|| probe.artwork(&payload.path));
                        match &result { Ok(_) => trace.event("artwork", "read"), Err(error) => trace.failed("artwork", error) }
                        let _ = payload.reply.send(result);
                    });
                } else { requests_closed = true; }
            }
            _ = thumbnails.join_next(), if !thumbnails.is_empty() => {}
        }
    }
    requests.close();
    if let Some((_, _, trace, task)) = reading {
        trace.event("artwork", "cancelled");
        task.abort();
    }
    thumbnails.abort_all();
    Ok(())
}
