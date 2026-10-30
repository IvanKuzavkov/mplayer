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
