use super::*;
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn rolling_logs_retain_two_files_and_flush_structured_events() {
    let directory = std::env::temp_dir().join(format!(
        "mplayer-logs-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    for day in 1..=4 {
        fs::write(
            directory.join(format!("mplayer.2000-01-0{day}.jsonl")),
            "old\n",
        )
        .unwrap();
    }
    fs::write(directory.join("unrelated.txt"), "keep").unwrap();
    let (writer, guard) = NonBlockingBuilder::default().finish(rolling_writer(&directory).unwrap());
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(writer)
        .with_span_events(FmtSpan::CLOSE)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("test_operation", operation_id = 42);
        let _entered = span.enter();
        tracing::info!(outcome = "success", "operation complete");
    });
    drop(guard);
    let logs: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect();
    assert!(logs.len() <= 2);
    let content: String = logs
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    assert!(content.contains("\"operation_id\":42"));
    assert!(content.contains("operation complete"));
    assert!(content.contains("time.busy"));
    assert!(directory.join("unrelated.txt").exists());
    fs::remove_dir_all(directory).unwrap();
}

#[derive(Clone, Default)]
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn command_traces_are_opt_in_and_follow_async_and_blocking_work() {
    use mplayer_core::{PlaybackCommand, TraceContext};
    use tracing::{Instrument, instrument::WithSubscriber};

    for enabled in [false, true] {
        let capture = Capture::default();
        let writer = capture.clone();
        let filter = if enabled {
            format!("{DEFAULT_FILTER},mplayer::commands=debug")
        } else {
            DEFAULT_FILTER.into()
        };
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_env_filter(EnvFilter::new(filter))
            .with_writer(move || writer.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let trace = tracing::dispatcher::with_default(&dispatch, || {
            TraceContext::new(&PlaybackCommand::PauseResume)
        });
        let id = trace.id.to_string();
        let span = trace.span.clone();
        let blocking_dispatch = dispatch.clone();
        tokio::spawn(
            async move {
                trace.event("playback", "handling");
                let next = TraceContext::new(&PlaybackCommand::Stop);
                trace.superseded_by(&next, "playback");
                tokio::task::spawn_blocking(move || {
                    tracing::dispatcher::with_default(&blocking_dispatch, || {
                        trace.span.in_scope(|| {
                            trace.event("audio", "handled");
                            trace.failed("audio", &"test error");
                        });
                    });
                })
                .await
                .unwrap();
            }
            .instrument(span)
            .with_subscriber(dispatch),
        )
        .await
        .unwrap();
        let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        if enabled {
            assert!(text.contains(&format!("\"trace_id\":\"{id}\"")));
            assert!(text.contains("\"level\":\"DEBUG\""));
            assert!(text.contains("\"component\":\"audio\""));
            assert!(text.contains("\"stage\":\"handled\""));
            assert!(text.contains("\"stage\":\"failed\""));
            assert!(text.contains("replacement_trace_id"));
        } else {
            assert!(
                text.is_empty(),
                "command DEBUG traces must respect the default filter"
            );
        }
    }
}
