use std::{sync::Arc, time::Duration};

use mplayer_core::OutputDevice;
use tokio::sync::watch;

pub(super) async fn run(
    devices: watch::Sender<Vec<OutputDevice>>,
    enumerate: impl Fn() -> Vec<OutputDevice> + Send + Sync + 'static,
    refresh_interval: Duration,
) {
    let enumerate = Arc::new(enumerate);
    // Dropping the set also cancels scans still queued in Tokio's blocking pool.
    // A native call that has already started must finish on its own.
    let mut scans = tokio::task::JoinSet::new();
    loop {
        if devices.is_closed() {
            return;
        }
        let enumerate = Arc::clone(&enumerate);
        let span = tracing::Span::current();
        // CPAL discovery calls blocking native APIs. Await each scan before
        // scheduling another, even when discovery takes longer than the interval.
        scans.spawn_blocking(move || {
            span.in_scope(|| {
                let started = std::time::Instant::now();
                let devices = enumerate();
                if started.elapsed() >= Duration::from_millis(50) {
                    tracing::warn!(
                        elapsed_ms = started.elapsed().as_millis(),
                        "Audio device discovery was slow"
                    );
                }
                devices
            })
        });
        let result = tokio::select! {
            _ = devices.closed() => return,
            Some(result) = scans.join_next() => result,
        };
        match result {
            Ok(found) => {
                devices.send_if_modified(|current| {
                    if *current == found {
                        false
                    } else {
                        *current = found;
                        true
                    }
                });
            }
            Err(error) => tracing::warn!(%error, "Audio device discovery task failed"),
        }
        tokio::select! {
            _ = devices.closed() => return,
            _ = tokio::time::sleep(refresh_interval) => (),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/discovery.rs"]
mod tests;
