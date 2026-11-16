use super::*;

#[test]
fn router_ids_are_unique_across_clones_and_retries_keep_the_original_id() {
    let (player, mut player_rx) = mpsc::channel(1);
    let (library, mut library_rx) = mpsc::channel(1);
    let (shutdown, _) = watch::channel(false);
    let router = CommandRouter::new(player, LibraryClient(library), shutdown);
    let mut ids = std::collections::HashSet::new();
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let router = router.clone();
            std::thread::spawn(move || {
                (0..100)
                    .map(|_| router.prepare(PlaybackCommand::Stop).trace.id)
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    for thread in threads {
        for id in thread.join().unwrap() {
            assert!(ids.insert(id));
        }
    }
    let first = router.prepare(PlaybackCommand::Stop);
    let id = first.trace.id;
    router.try_send(first).unwrap();
    let second = router.prepare(PlaybackCommand::Next);
    let retry_id = second.trace.id;
    let error = router.try_send(second).unwrap_err();
    assert!(matches!(error, mpsc::error::TrySendError::Full(_)));
    let retry = error.into_inner();
    assert_eq!(retry.trace.id, retry_id);
    // A full playback inbox does not prevent independent library delivery.
    router
        .try_send(router.prepare(LibraryCommand::Rescan))
        .unwrap();
    assert!(matches!(
        library_rx.try_recv().unwrap().payload,
        LibraryRequest::Command(LibraryCommand::Rescan)
    ));
    assert_eq!(player_rx.try_recv().unwrap().trace.id, id);
    router.try_send(retry).unwrap();
    assert_eq!(player_rx.try_recv().unwrap().trace.id, retry_id);
    drop(player_rx);
    let closed = router.prepare(PlaybackCommand::Stop);
    let closed_id = closed.trace.id;
    let error = router.try_send(closed).unwrap_err();
    assert!(matches!(error, mpsc::error::TrySendError::Closed(_)));
    assert_eq!(error.into_inner().trace.id, closed_id);
}

#[tokio::test]
async fn snapshots_are_independent_quiet_and_keep_the_final_value() {
    let (sender, mut first) = watch::channel(PlayerSnapshot::default());
    let mut second = first.clone();
    publish_changed(&sender, &PlayerSnapshot::default());
    assert!(!first.has_changed().unwrap());
    let value = PlayerSnapshot {
        status: "Final state".into(),
        ..Default::default()
    };
    publish_changed(&sender, &value);
    first.changed().await.unwrap();
    assert_eq!(*first.borrow_and_update(), value);
    assert!(second.has_changed().unwrap());
    let late = sender.subscribe();
    assert_eq!(*late.borrow(), value);
    drop(sender);
    assert_eq!(*second.borrow_and_update(), value);
    assert!(second.changed().await.is_err());
}

#[tokio::test]
async fn shutdown_bypasses_full_inboxes_and_rejects_new_commands() {
    let (player, _player_rx) = mpsc::channel(1);
    let (library, _library_rx) = mpsc::channel(1);
    let (shutdown, mut stop) = watch::channel(false);
    let router = CommandRouter::new(player, LibraryClient(library), shutdown);
    router
        .try_send(router.prepare(PlaybackCommand::Stop))
        .unwrap();
    router
        .try_send(router.prepare(LibraryCommand::Rescan))
        .unwrap();
    router
        .try_send(router.prepare(AppCommand::Shutdown))
        .unwrap();
    shutdown_requested(&mut stop).await;
    assert!(router.playback_closed());
    assert!(router.library_closed());
    assert!(matches!(
        router.try_send(router.prepare(PlaybackCommand::Next)),
        Err(mpsc::error::TrySendError::Closed(_))
    ));
}
