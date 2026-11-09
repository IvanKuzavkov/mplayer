use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(5);

#[tokio::test]
async fn slow_scans_do_not_block_tokio_or_overlap_and_publish_completed_lists() {
    let (sender, mut devices) = watch::channel(Vec::new());
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let (release, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    let calls = Arc::new(AtomicUsize::new(0));
    let scans = Arc::clone(&calls);
    let task = tokio::spawn(run(
        sender,
        move || {
            let call = scans.fetch_add(1, Ordering::Relaxed) + 1;
            started.send(call).unwrap();
            gate.lock().unwrap().recv_timeout(DEADLINE).unwrap();
            vec![format!("Output {call}")]
        },
        Duration::from_millis(10),
    ));

    assert_eq!(timeout(DEADLINE, starts.recv()).await.unwrap(), Some(1));
    assert!(devices.borrow().is_empty());
    // The async executor remains responsive while the native call is blocked.
    assert!(
        timeout(Duration::from_millis(30), starts.recv())
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    release.send(()).unwrap();
    timeout(DEADLINE, devices.changed()).await.unwrap().unwrap();
    assert_eq!(*devices.borrow_and_update(), ["Output 1"]);
    assert_eq!(timeout(DEADLINE, starts.recv()).await.unwrap(), Some(2));
    assert_eq!(*devices.borrow(), ["Output 1"]);
    release.send(()).unwrap();
    timeout(DEADLINE, devices.changed()).await.unwrap().unwrap();
    assert_eq!(*devices.borrow_and_update(), ["Output 2"]);
    drop(devices);
    timeout(DEADLINE, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn unchanged_lists_are_not_republished_and_idle_discovery_stops_on_close() {
    let (sender, mut devices) = watch::channel(vec!["Output".to_owned()]);
    let (completed, mut completions) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(run(
        sender,
        move || {
            completed.send(()).unwrap();
            vec!["Output".to_owned()]
        },
        Duration::from_millis(10),
    ));

    // Starting the next scan proves the preceding result has been processed.
    for _ in 0..2 {
        timeout(DEADLINE, completions.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(!devices.has_changed().unwrap());
    assert_eq!(*devices.borrow_and_update(), ["Output"]);
    drop(devices);
    timeout(DEADLINE, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn closing_during_a_blocked_scan_stops_discovery_without_waiting_for_it() {
    let (sender, devices) = watch::channel(Vec::new());
    let (started, ready) = tokio::sync::oneshot::channel();
    let started = Mutex::new(Some(started));
    let (release, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    let task = tokio::spawn(run(
        sender,
        move || {
            started.lock().unwrap().take().unwrap().send(()).unwrap();
            gate.lock().unwrap().recv_timeout(DEADLINE).unwrap();
            Vec::new()
        },
        Duration::from_secs(3),
    ));

    timeout(DEADLINE, ready).await.unwrap().unwrap();
    drop(devices);
    let stopped = timeout(DEADLINE, task).await;
    // Running spawn_blocking calls cannot be cancelled; let the fake native call exit.
    release.send(()).unwrap();
    stopped.unwrap().unwrap();
}
