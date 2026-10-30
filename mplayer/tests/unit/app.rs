use super::*;
#[path = "albums.rs"]
mod album_tests;
#[path = "queue.rs"]
mod queue_tests;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, sync_channel},
    },
};

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn play_modes_control_prefetch_automatic_advancement_and_manual_next() {
    for mode in [PlayMode::Sequential, PlayMode::Repeat, PlayMode::Shuffle] {
        let mut h = Harness::new().await;
        h.app.state.queue = vec![track(1), track(2), track(3)].into();
        h.app.handle(AppCommand::SetPlayMode(mode)).await.unwrap();
        h.audio.try_iter().for_each(drop);
        h.app.play_index(2).unwrap();
        let generation = h.app.generation;
        let AudioCommand::Load { next, .. } = h.audio.try_recv().unwrap() else {
            panic!("expected load");
        };
        let selected = h.app.next_queue_index;
        match mode {
            PlayMode::Sequential => assert!(next.is_none()),
            PlayMode::Repeat => assert_eq!(selected, Some(0)),
            PlayMode::Shuffle => assert!(selected.is_some_and(|index| index < 2)),
        }
        h.playback.send_replace(PlaybackSnapshot {
            generation,
            completed: 1,
            track: next.clone().or(Some(track(3))),
            state: if next.is_some() {
                PlaybackState::Playing
            } else {
                PlaybackState::Stopped
            },
            ..Default::default()
        });
        h.app.update_playback().unwrap();
        assert_eq!(h.app.state.queue_index, selected.or(Some(2)));
        assert_eq!(h.app.generation, generation);
        if let Some(index) = h.app.next_queue_index {
            assert!(
                matches!(h.audio.try_recv().unwrap(), AudioCommand::SetNext(Some(next)) if next == h.app.state.queue[index])
            );
            h.app.handle(AppCommand::Next).await.unwrap();
            assert_eq!(h.app.state.queue_index, Some(index));
            assert!(
                matches!(h.audio.try_recv().unwrap(), AudioCommand::Load { track, .. } if track == h.app.state.queue[index])
            );
        } else {
            h.app.handle(AppCommand::Next).await.unwrap();
            assert!(h.audio.try_recv().is_err());
        }
        h.app.store.close().await;
    }
}

#[tokio::test]
async fn coalesced_completions_stop_sequential_and_restart_repeat_and_shuffle() {
    for mode in [PlayMode::Sequential, PlayMode::Repeat, PlayMode::Shuffle] {
        let mut h = Harness::new().await;
        h.app.state.play_mode = mode;
        h.app.state.queue = vec![track(1), track(2)].into();
        h.app.play_index(0).unwrap();
        h.audio.try_iter().for_each(drop);
        h.playback.send_replace(PlaybackSnapshot {
            generation: h.app.generation,
            completed: 2,
            track: Some(track(2)),
            state: PlaybackState::Stopped,
            ..Default::default()
        });
        h.app.update_playback().unwrap();
        if mode == PlayMode::Sequential {
            assert_eq!(h.app.state.queue_index, Some(1));
            assert!(h.audio.try_recv().is_err());
        } else {
            assert_eq!(h.app.state.queue_index, Some(0));
            assert!(
                matches!(h.audio.try_recv().unwrap(), AudioCommand::Load { track, next: Some(next), .. } if track.id == 1 && next.id == 2)
            );
        }
        h.app.store.close().await;
    }
}

#[tokio::test]
async fn play_mode_changes_preserve_playback_and_retry_prefetch_under_backpressure() {
    let mut h = Harness::new().await;
    h.app.state.queue = vec![track(1), track(2)].into();
    h.app.play_index(1).unwrap();
    h.app.state.playback.position_ms = 4321;
    h.app.state.playback.state = PlaybackState::Paused;
    let generation = h.app.generation;
    h.audio.try_iter().for_each(drop);
    for _ in 0..64 {
        h.app.audio.try_send(AudioCommand::Pause).unwrap();
    }
    h.app
        .handle(AppCommand::SetPlayMode(PlayMode::Repeat))
        .await
        .unwrap();
    assert_eq!(h.app.state.play_mode, PlayMode::Repeat);
    assert_eq!(h.app.state.queue_index, Some(1));
    assert_eq!(h.app.state.playback.position_ms, 4321);
    assert_eq!(h.app.state.playback.state, PlaybackState::Paused);
    assert_eq!(h.app.generation, generation);
    h.audio.try_iter().for_each(drop);
    h.app.flush_audio().unwrap();
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::SetNext(Some(next)) if next.id == 1)
    );
    h.app
        .handle(AppCommand::SetPlayMode(PlayMode::Sequential))
        .await
        .unwrap();
    assert!(matches!(
        h.audio.try_recv().unwrap(),
        AudioCommand::SetNext(None)
    ));
    h.app.store.close().await;
}

#[tokio::test]
async fn mode_selection_handles_empty_singleton_and_edited_queues() {
    let mut h = Harness::new().await;
    for mode in [PlayMode::Sequential, PlayMode::Repeat, PlayMode::Shuffle] {
        assert_eq!(h.app.choose_next(mode, 0, 0), None);
        assert_eq!(h.app.choose_next(mode, 2, 2), None);
        assert_eq!(
            h.app.choose_next(mode, 0, 1),
            if mode == PlayMode::Sequential {
                None
            } else {
                Some(0)
            }
        );
        h.app.state.play_mode = mode;
        h.app
            .load_queue(vec![track(1), track(2), track(3)].into(), 1)
            .unwrap();
        h.audio.try_iter().for_each(drop);
        h.app.handle(AppCommand::RemoveQueue(0)).await.unwrap();
        assert_eq!(h.app.state.queue_index, Some(0));
        let expected = h
            .app
            .next_queue_index
            .map(|index| h.app.state.queue[index].clone());
        assert!(
            matches!(h.audio.try_recv().unwrap(), AudioCommand::SetNext(next) if next == expected)
        );
        h.app.handle(AppCommand::ClearQueue).await.unwrap();
        h.audio.try_iter().for_each(drop);
        assert!(h.app.next_queue_index.is_none());
    }
    let mut seen = std::collections::HashSet::new();
    for _ in 0..128 {
        let index = h.app.choose_next(PlayMode::Shuffle, 2, 5).unwrap();
        assert!(index < 5 && index != 2);
        seen.insert(index);
    }
    assert_eq!(seen.len(), 4);
    h.app.store.close().await;
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let id = DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("mplayer-app-{}-{nanos}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Probe;

impl MetadataProbe for Probe {
    fn probe(&self, path: &Path) -> Result<TrackMetadata> {
        if path.file_stem().is_some_and(|name| name == "broken") {
            anyhow::bail!("malformed fixture");
        }
        Ok(TrackMetadata {
            title: path.file_stem().unwrap().to_string_lossy().into_owned(),
            duration_ms: 10_000,
            sample_rate: 44_100,
            channels: 2,
            ..Default::default()
        })
    }
}

struct Harness {
    _directory: TempDirectory,
    app: Application,
    audio: Receiver<AudioCommand>,
    playback: watch::Sender<PlaybackSnapshot>,
    commands: mpsc::Sender<AppCommand>,
    snapshots: watch::Receiver<AppSnapshot>,
    _logging_guard: tracing_appender::non_blocking::WorkerGuard,
}

impl Harness {
    async fn new() -> Self {
        let directory = TempDirectory::new();
        let store = Store::open(&directory.0.join("library.sqlite"))
            .await
            .unwrap();
        let (audio_sender, audio) = sync_channel(64);
        let (playback, playback_receiver) = watch::channel(PlaybackSnapshot::default());
        let (commands, receiver) = mpsc::channel(64);
        let (output, snapshots) = watch::channel(AppSnapshot::default());
        let (writer, logging_guard) = tracing_appender::non_blocking(std::io::sink());
        let app = Application::new(
            store,
            audio_sender,
            playback_receiver,
            receiver,
            output,
            Arc::new(|| {}),
            Arc::new(Probe),
            writer.error_counter(),
        );
        Self {
            _directory: directory,
            app,
            audio,
            playback,
            commands,
            snapshots,
            _logging_guard: logging_guard,
        }
    }
}

fn track(id: i64) -> Track {
    Track {
        id,
        path: PathBuf::from(format!("track-{id}.flac")),
        missing: false,
        segment: None,
        metadata: TrackMetadata {
            title: format!("Track {id}"),
            duration_ms: 10_000,
            sample_rate: 44_100,
            channels: 2,
            ..Default::default()
        },
    }
}

async fn wait_for(
    snapshot: &mut watch::Receiver<AppSnapshot>,
    predicate: impl Fn(&AppSnapshot) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if predicate(&snapshot.borrow()) {
                return;
            }
            snapshot
                .changed()
                .await
                .expect("application exited before publishing expected state");
        }
    })
    .await
    .expect("application did not publish expected state");
}

#[test]
fn queue_removal_preserves_current_track_and_handles_last_entry() {
    assert_eq!(index_after_removal(Some(2), 0, 3), Some(1));
    assert_eq!(index_after_removal(Some(1), 2, 3), Some(1));
    assert_eq!(index_after_removal(Some(2), 2, 2), Some(1));
    assert_eq!(index_after_removal(Some(0), 0, 0), None);
    assert_eq!(index_after_removal(None, 0, 2), None);
}

#[tokio::test]
async fn new_playlists_open_immediately_and_deletion_returns_to_the_list() {
    let mut h = Harness::new().await;
    h.app
        .handle(AppCommand::CreatePlaylist("  First  ".into()))
        .await
        .unwrap();
    let first = h.app.state.selected_playlist.unwrap();
    assert_eq!(h.app.state.playlists[0].name, "First");
    assert!(
        h.app
            .handle(AppCommand::CreatePlaylist(" first ".into()))
            .await
            .is_err()
    );
    assert_eq!(h.app.state.selected_playlist, Some(first));
    assert_eq!(h.app.state.playlists.len(), 1);
    h.app
        .handle(AppCommand::CreatePlaylist("Second".into()))
        .await
        .unwrap();
    let second = h.app.state.selected_playlist.unwrap();
    assert_ne!(first, second);
    assert!(h.app.state.playlist_entries.is_empty());
    h.app
        .handle(AppCommand::DeletePlaylist(second))
        .await
        .unwrap();
    assert_eq!(h.app.state.selected_playlist, None);
    assert_eq!(h.app.state.playlists.len(), 1);
    assert_eq!(h.app.state.playlists[0].id, first);
    h.app.store.close().await;
}

#[tokio::test]
async fn removing_a_root_rejects_running_and_queued_scans() {
    let mut h = Harness::new().await;
    h.app.scan_task = Some(tokio::spawn(std::future::pending()));
    assert!(
        h.app
            .handle(AppCommand::RemoveScanFolder("/music".into()))
            .await
            .is_err()
    );
    h.app.scan_task.take().unwrap().abort();
    h.app.pending_imports.push("/music".into());
    assert!(
        h.app
            .handle(AppCommand::RemoveScanFolder("/music".into()))
            .await
            .is_err()
    );
    h.app.pending_imports.clear();
    h.app
        .handle(AppCommand::RemoveScanFolder("/music".into()))
        .await
        .unwrap();
    assert_eq!(
        h.app.state.status,
        "Music folder removed; files kept on disk"
    );
    h.app.store.close().await;
}

#[tokio::test]
async fn disconnected_output_notification_resumes_without_reloading_track() {
    let mut h = Harness::new().await;
    h.app.state.queue = vec![track(1), track(2)].into();
    h.app.play_index(0).unwrap();
    let generation = h.app.generation;
    h.audio.try_iter().for_each(drop);
    let message = "Audio output device changed. Playback paused.";
    h.playback.send_replace(PlaybackSnapshot {
        generation,
        track: Some(track(1)),
        state: PlaybackState::Paused,
        position_ms: 12_000,
        notification: Some(message.into()),
        ..Default::default()
    });
    h.app.update_playback().unwrap();
    assert_eq!(h.app.state.status, message);
    assert!(h.app.state.playback.error.is_none());

    h.app.handle(AppCommand::SetDevice(None)).await.unwrap();
    h.audio.try_iter().for_each(drop);
    h.app.handle(AppCommand::PauseResume).await.unwrap();
    assert!(matches!(h.audio.try_recv().unwrap(), AudioCommand::Resume));
    assert!(h.audio.try_recv().is_err());
    assert_eq!(h.app.generation, generation);
    assert_eq!(h.app.state.playback.position_ms, 12_000);

    h.playback.send_replace(PlaybackSnapshot {
        generation,
        track: Some(track(1)),
        state: PlaybackState::Playing,
        position_ms: 12_000,
        ..Default::default()
    });
    h.app.update_playback().unwrap();
    assert_eq!(h.app.state.status, "Ready");
    h.app.store.close().await;
}

#[tokio::test]
async fn manual_next_ignores_stale_completion_without_losing_new_completion() {
    let mut h = Harness::new().await;
    h.app.state.queue = vec![track(1), track(2), track(3)].into();
    h.app.play_index(0).unwrap();
    let old_generation = h.app.generation;
    h.app.handle(AppCommand::Next).await.unwrap();
    let new_generation = h.app.generation;
    assert_ne!(old_generation, new_generation);
    h.audio.try_iter().for_each(drop);

    // A completed old track can still publish after a manual Next command.
    h.playback.send_replace(PlaybackSnapshot {
        generation: old_generation,
        completed: 1,
        track: Some(track(2)),
        state: PlaybackState::Playing,
        ..Default::default()
    });
    h.app.update_playback().unwrap();
    assert_eq!(h.app.state.queue_index, Some(1));
    assert_eq!(h.app.completed, 0);
    assert!(h.audio.try_recv().is_err());

    // The first observable new-generation snapshot may already contain a
    // completion, because watch intentionally coalesces intermediate updates.
    h.playback.send_replace(PlaybackSnapshot {
        generation: new_generation,
        completed: 1,
        track: Some(track(3)),
        state: PlaybackState::Playing,
        ..Default::default()
    });
    h.app.update_playback().unwrap();
    assert_eq!(h.app.state.queue_index, Some(2));
    assert_eq!(h.app.generation, new_generation);
    assert!(matches!(
        h.audio.try_recv().unwrap(),
        AudioCommand::SetNext(None)
    ));
    assert!(h.audio.try_recv().is_err());
    h.app.store.close().await;
}

#[tokio::test]
async fn duplicate_queue_entries_advance_by_completion_and_replacement_invalidates_old_events() {
    let mut h = Harness::new().await;
    h.app.state.queue = vec![track(1), track(1), track(2)].into();
    h.app.play_index(0).unwrap();
    let old_generation = h.app.generation;
    h.audio.try_iter().for_each(drop);
    h.playback.send_replace(PlaybackSnapshot {
        generation: old_generation,
        completed: 1,
        track: Some(track(1)),
        state: PlaybackState::Playing,
        ..Default::default()
    });
    h.app.update_playback().unwrap();
    assert_eq!(h.app.state.queue_index, Some(1));
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::SetNext(Some(next)) if next.id == 2)
    );

    h.app.handle(AppCommand::Play(track(7))).await.unwrap();
    h.audio.try_iter().for_each(drop);
    h.playback.send_replace(PlaybackSnapshot {
        generation: old_generation,
        completed: 2,
        track: Some(track(2)),
        state: PlaybackState::Playing,
        ..Default::default()
    });
    h.app.update_playback().unwrap();
    assert_eq!(h.app.state.queue.as_ref(), &vec![track(7)]);
    assert_eq!(h.app.state.queue_index, Some(0));
    assert!(h.audio.try_recv().is_err());
    h.app.store.close().await;
}

#[tokio::test]
async fn stop_invalidates_in_flight_completion_without_restarting_queue() {
    let mut h = Harness::new().await;
    h.app.state.queue = vec![track(1), track(2)].into();
    h.app.play_index(0).unwrap();
    let old_generation = h.app.generation;
    h.audio.try_iter().for_each(drop);
    h.app.handle(AppCommand::Stop).await.unwrap();
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::Stop { generation } if generation != old_generation)
    );
    h.playback.send_replace(PlaybackSnapshot {
        generation: old_generation,
        completed: 1,
        track: Some(track(2)),
        state: PlaybackState::Playing,
        ..Default::default()
    });
    h.app.update_playback().unwrap();
    assert_eq!(h.app.state.queue_index, Some(0));
    assert!(h.audio.try_recv().is_err());
    h.app.store.close().await;
}

#[tokio::test]
async fn rejected_play_command_preserves_queue_and_generation() {
    let mut h = Harness::new().await;
    h.app.state.queue = vec![track(1)].into();
    h.app.state.queue_index = Some(0);
    let (sender, _receiver) = sync_channel(1);
    sender.try_send(AudioCommand::SetVolume(1.0)).unwrap();
    h.app.audio = sender;
    h.app.pending_audio = (0..64).map(|_| AudioCommand::Pause).collect();
    let generation = h.app.generation;
    assert!(
        h.app
            .handle(AppCommand::Play(track(2)))
            .await
            .unwrap_err()
            .to_string()
            .contains("busy")
    );
    assert_eq!(h.app.state.queue.as_ref(), &vec![track(1)]);
    assert_eq!(h.app.state.queue_index, Some(0));
    assert_eq!(h.app.generation, generation);
    h.app.store.close().await;
}

#[tokio::test]
async fn full_audio_channel_retries_in_order_and_coalesces_seeks() {
    let mut h = Harness::new().await;
    let (sender, receiver) = sync_channel(1);
    sender.try_send(AudioCommand::SetVolume(0.9)).unwrap();
    h.app.audio = sender;
    h.app.send_audio(AudioCommand::Pause).unwrap();
    h.app
        .send_audio(AudioCommand::Seek {
            position_ms: 10,
            request_id: 10,
        })
        .unwrap();
    h.app
        .send_audio(AudioCommand::Seek {
            position_ms: 20,
            request_id: 20,
        })
        .unwrap();
    assert_eq!(h.app.pending_audio.len(), 2);
    assert!(matches!(
        receiver.try_recv().unwrap(),
        AudioCommand::SetVolume(_)
    ));
    h.app.flush_audio().unwrap();
    assert!(matches!(receiver.try_recv().unwrap(), AudioCommand::Pause));
    h.app.flush_audio().unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        AudioCommand::Seek {
            position_ms: 20,
            request_id: 20
        }
    ));
    assert!(h.app.pending_audio.is_empty());
    assert!(receiver.try_recv().is_err());
    h.app.store.close().await;
}

#[tokio::test]
async fn stale_audio_volume_acknowledgment_does_not_overwrite_requested_volume() {
    let mut h = Harness::new().await;
    h.app.handle(AppCommand::SetVolume(0.35)).await.unwrap();
    h.playback.send_replace(PlaybackSnapshot {
        volume: 1.0,
        generation: h.app.generation,
        ..Default::default()
    });
    h.app.update_playback().unwrap();
    h.app.persist_volume().await.unwrap();
    assert_eq!(h.app.state.playback.volume, 0.35);
    assert_eq!(
        h.app.store.get_setting("volume").await.unwrap().as_deref(),
        Some("0.35")
    );
    h.app.store.close().await;
}

#[tokio::test]
async fn settings_restore_and_volume_validation_use_persisted_values() {
    let mut h = Harness::new().await;
    h.app.store.set_setting("volume", "NaN").await.unwrap();
    h.app
        .store
        .set_setting("output_device", "Test DAC")
        .await
        .unwrap();
    h.app.restore().await.unwrap();
    assert_eq!(h.app.state.playback.state, PlaybackState::Stopped);
    assert!(matches!(h.audio.try_recv().unwrap(), AudioCommand::SetVolume(value) if value == 1.0));
    assert!(
        matches!(h.audio.try_recv().unwrap(), AudioCommand::SetDevice(Some(name)) if name == "Test DAC")
    );
    h.app.handle(AppCommand::SetVolume(f32::NAN)).await.unwrap();
    assert!(h.audio.try_recv().is_err());
    assert!(!h.app.volume_dirty);
    h.app.handle(AppCommand::SetVolume(-0.4)).await.unwrap();
    h.app.persist_volume().await.unwrap();
    assert_eq!(
        h.app.store.get_setting("volume").await.unwrap().as_deref(),
        Some("0")
    );
    assert!(!h.app.volume_dirty);
    h.app.store.close().await;
}

#[tokio::test]
async fn final_scan_progress_is_published_after_progress_sender_closes() {
    let mut h = Harness::new().await;
    let final_progress = ScanProgress {
        running: false,
        discovered: 12,
        processed: 12,
        errors: 2,
        folder_revision: 1,
    };
    let (sender, receiver) = watch::channel(ScanProgress {
        running: true,
        ..Default::default()
    });
    sender.send_replace(final_progress.clone());
    drop(sender);
    h.app.scan_progress = Some(receiver);
    h.app.scan_task = Some(tokio::spawn(async { Ok(()) }));
    let store = h.app.store.clone();
    let task = tokio::spawn(h.app.run());
    wait_for(&mut h.snapshots, |state| {
        state.status.starts_with("Scan complete:")
    })
    .await;
    assert_eq!(h.snapshots.borrow().scan, final_progress);
    assert!(h.snapshots.borrow().status.contains("12 files, 2 errors"));
    h.commands.send(AppCommand::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    store.close().await;
}

#[tokio::test]
async fn import_scan_publishes_folder_error_and_targeted_retry() {
    let mut h = Harness::new().await;
    let root = h._directory.0.join("music");
    std::fs::create_dir_all(&root).unwrap();
    for name in ["first.flac", "second.mp3", "third.ogg", "broken.wav"] {
        std::fs::write(root.join(name), b"fixture").unwrap();
    }
    let store = h.app.store.clone();
    let task = tokio::spawn(h.app.run());
    h.commands
        .send(AppCommand::Import(vec![root.clone()]))
        .await
        .unwrap();
    wait_for(&mut h.snapshots, |state| {
        state.status.starts_with("Scan complete:")
    })
    .await;
    let snapshot = h.snapshots.borrow().clone();
    assert_eq!(snapshot.library.total, 0);
    assert!((1..=4).contains(&snapshot.scan.processed));
    let folder = &snapshot.folders.folders[0];
    assert_eq!(folder.status, ScanStatus::Error);
    assert!(folder.error.as_ref().unwrap().contains("malformed fixture"));
    let folder_id = folder.id;
    assert_eq!(snapshot.scan.errors, 1);
    assert!(!snapshot.scan.running);
    assert_eq!(snapshot.roots.len(), 1);
    std::fs::remove_file(root.join("broken.wav")).unwrap();
    h.commands
        .send(AppCommand::RescanFolder(folder_id))
        .await
        .unwrap();
    wait_for(&mut h.snapshots, |state| {
        state.library.total == 3 && !state.scan.running
    })
    .await;
    let retried = h.snapshots.borrow().clone();
    assert_eq!(retried.folders.folders[0].status, ScanStatus::Ready);
    assert_eq!(retried.folders.folders[0].generation, 1);
    h.commands.send(AppCommand::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    store.close().await;
}

#[tokio::test]
async fn folders_added_during_a_scan_are_imported_automatically_after_it_finishes() {
    let mut h = Harness::new().await;
    let first = h._directory.0.join("first-folder");
    let second = h._directory.0.join("second-folder");
    for root in [&first, &second] {
        tokio::fs::create_dir_all(root).await.unwrap();
        tokio::fs::write(root.join("track.flac"), b"fixture")
            .await
            .unwrap();
    }
    let (finish, finished) = tokio::sync::oneshot::channel();
    h.app.scan_task = Some(tokio::spawn(async move {
        finished.await?;
        Ok(())
    }));
    h.app.state.scan.running = true;
    let store = h.app.store.clone();
    let task = tokio::spawn(h.app.run());
    h.commands
        .send(AppCommand::Import(vec![first.clone(), second, first]))
        .await
        .unwrap();
    wait_for(&mut h.snapshots, |state| state.queued_imports == 2).await;
    assert!(h.snapshots.borrow().scan.running);
    finish.send(()).unwrap();
    wait_for(&mut h.snapshots, |state| {
        state.library.total == 2 && !state.scan.running
    })
    .await;
    assert_eq!(h.snapshots.borrow().queued_imports, 0);
    assert_eq!(h.snapshots.borrow().roots.len(), 2);
    h.commands.send(AppCommand::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    store.close().await;
}

#[tokio::test]
async fn cancelling_scans_clears_folders_queued_by_the_ui() {
    let mut h = Harness::new().await;
    let release = start_blocked_scan(&mut h).await;
    h.app
        .handle(AppCommand::Import(vec![PathBuf::from("another-folder")]))
        .await
        .unwrap();
    assert_eq!(h.app.state.queued_imports, 1);
    h.app.handle(AppCommand::CancelScan).await.unwrap();
    assert_eq!(h.app.state.queued_imports, 0);
    assert!(h.app.pending_imports.is_empty());
    assert!(h.app.scan_cancel.is_none());
    h.app.handle(AppCommand::CancelScan).await.unwrap();
    assert!(h.app.scan_task.take().unwrap().await.unwrap().is_err());
    assert!(!h.app.scan_progress.as_ref().unwrap().borrow().running);
    let _ = release.send(());
    h.app.store.close().await;
}

#[tokio::test]
async fn full_scan_queue_rejects_new_paths_without_losing_accepted_folders() {
    let mut h = Harness::new().await;
    h.app.scan_task = Some(tokio::spawn(std::future::pending()));
    let paths: Vec<_> = (0..64)
        .map(|index| PathBuf::from(format!("folder-{index}")))
        .collect();
    h.app
        .handle(AppCommand::Import(paths.clone()))
        .await
        .unwrap();
    assert!(
        h.app
            .handle(AppCommand::Import(vec![PathBuf::from("overflow")]))
            .await
            .is_err()
    );
    assert_eq!(h.app.pending_imports, paths);
    assert_eq!(h.app.state.queued_imports, 64);
    h.app.scan_task.take().unwrap().abort();
    h.app.store.close().await;
}

#[tokio::test]
async fn shutdown_flushes_volume_and_cooperatively_joins_scan() {
    let mut h = Harness::new().await;
    let release = start_blocked_scan(&mut h).await;
    let progress = h.app.scan_progress.as_ref().unwrap().clone();
    let store = h.app.store.clone();
    let task = tokio::spawn(h.app.run());
    h.commands.send(AppCommand::SetVolume(0.35)).await.unwrap();
    wait_for(&mut h.snapshots, |state| state.playback.volume == 0.35).await;
    h.commands.send(AppCommand::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!progress.borrow().running);
    assert!(progress.has_changed().is_err());
    let _ = release.send(());
    assert_eq!(
        store.get_setting("volume").await.unwrap().as_deref(),
        Some("0.35")
    );
    store.close().await;
}

async fn start_blocked_scan(h: &mut Harness) -> std::sync::mpsc::Sender<()> {
    struct BlockingProbe {
        started: Arc<tokio::sync::Notify>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl MetadataProbe for BlockingProbe {
        fn probe(&self, _: &Path) -> Result<TrackMetadata> {
            self.started.notify_one();
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3));
            Ok(TrackMetadata::default())
        }
    }
    let root = h._directory.0.join("blocked-scan");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("track.flac"), b"fixture").unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let (release, receiver) = std::sync::mpsc::channel();
    h.app.probe = Arc::new(BlockingProbe {
        started: Arc::clone(&started),
        release: std::sync::Mutex::new(receiver),
    });
    h.app.start_scan(vec![root]).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    assert!(h.app.scan_cancel.is_some());
    assert!(!h.app.scan_task.as_ref().unwrap().is_finished());
    release
}

#[tokio::test]
async fn cancelled_scan_publishes_final_progress_and_cancelled_status() {
    let mut h = Harness::new().await;
    let release = start_blocked_scan(&mut h).await;
    let store = h.app.store.clone();
    let task = tokio::spawn(h.app.run());
    h.commands.send(AppCommand::CancelScan).await.unwrap();
    wait_for(&mut h.snapshots, |state| state.status == "Scan cancelled").await;
    assert!(!h.snapshots.borrow().scan.running);
    h.commands.send(AppCommand::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let _ = release.send(());
    store.close().await;
}

#[tokio::test]
async fn rapid_skips_keep_artwork_reads_bounded_and_discard_stale_results() {
    use std::sync::atomic::AtomicUsize;
    #[derive(Default)]
    struct SlowProbe {
        starts: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
        released: AtomicBool,
    }
    impl MetadataProbe for SlowProbe {
        fn probe(&self, _: &Path) -> Result<TrackMetadata> {
            Ok(TrackMetadata::default())
        }
        fn artwork(&self, path: &Path) -> Result<Option<Artwork>> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            while !self.released.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(Some(Artwork {
                data: path.to_string_lossy().as_bytes().into(),
                mime_type: "test/data".into(),
            }))
        }
    }
    struct ReleaseOnDrop(Arc<SlowProbe>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.released.store(true, Ordering::SeqCst);
        }
    }
    let mut h = Harness::new().await;
    let probe = Arc::new(SlowProbe::default());
    let _release = ReleaseOnDrop(Arc::clone(&probe));
    h.app.probe = probe.clone();
    h.app.state.playback.track = Some(track(1));
    h.app.refresh_artwork();
    tokio::time::timeout(Duration::from_secs(3), async {
        while probe.starts.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    for id in 2..100 {
        h.app.state.playback.track = Some(track(id));
        h.app.refresh_artwork();
    }
    assert_eq!(probe.starts.load(Ordering::SeqCst), 1);
    probe.released.store(true, Ordering::SeqCst);
    h.app.finish_artwork().await;
    assert!(
        h.app.state.artwork.is_none(),
        "old track cover must not be published"
    );
    h.app.finish_artwork().await;
    let artwork = h.app.state.artwork.as_ref().unwrap();
    assert_eq!(&*artwork.data, b"track-99.flac");
    assert_eq!(probe.starts.load(Ordering::SeqCst), 2);
    assert_eq!(probe.peak.load(Ordering::SeqCst), 1);
    h.app.store.close().await;
}

#[tokio::test]
async fn artwork_reads_keep_the_current_cover_until_replacement_or_confirmed_absence() {
    struct ArtworkProbe;
    impl MetadataProbe for ArtworkProbe {
        fn probe(&self, _: &Path) -> Result<TrackMetadata> {
            Ok(TrackMetadata::default())
        }
        fn artwork(&self, path: &Path) -> Result<Option<Artwork>> {
            if path == Path::new("track-3.flac") {
                return Ok(None);
            }
            if path == Path::new("track-4.flac") {
                anyhow::bail!("Artwork read failed");
            }
            Ok(Some(Artwork {
                data: path.to_string_lossy().as_bytes().into(),
                mime_type: "test/data".into(),
            }))
        }
    }
    let mut h = Harness::new().await;
    h.app.probe = Arc::new(ArtworkProbe);
    h.app.state.playback.track = Some(track(1));
    h.app.refresh_artwork();
    h.app.finish_artwork().await;
    let first = h.app.state.artwork.as_ref().unwrap().data.clone();
    h.app.state.playback.track = Some(track(2));
    h.app.refresh_artwork();
    assert!(Arc::ptr_eq(
        &h.app.state.artwork.as_ref().unwrap().data,
        &first
    ));
    h.app.finish_artwork().await;
    assert_eq!(
        h.app.state.artwork.as_ref().unwrap().data.as_ref(),
        b"track-2.flac"
    );
    for id in [3, 4] {
        h.app.state.playback.track = Some(track(1));
        h.app.refresh_artwork();
        h.app.finish_artwork().await;
        let previous = h.app.state.artwork.clone();
        h.app.state.playback.track = Some(track(id));
        h.app.refresh_artwork();
        assert_eq!(
            h.app.state.artwork.as_ref().map(|art| &art.data),
            previous.as_ref().map(|art| &art.data)
        );
        h.app.finish_artwork().await;
        assert!(h.app.state.artwork.is_none());
    }
    h.app.state.playback.track = Some(track(1));
    h.app.refresh_artwork();
    h.app.finish_artwork().await;
    h.app.state.playback.track = None;
    h.app.refresh_artwork();
    assert!(h.app.state.artwork.is_none());
    h.app.store.close().await;
}

#[tokio::test]
async fn seek_request_identity_survives_forwarding_and_snapshot_updates() {
    let mut h = Harness::new().await;
    h.app
        .handle(AppCommand::Seek {
            position_ms: 5000,
            request_id: 17,
        })
        .await
        .unwrap();
    assert!(matches!(
        h.audio.try_recv().unwrap(),
        AudioCommand::Seek {
            position_ms: 5000,
            request_id: 17
        }
    ));
    h.playback.send_replace(PlaybackSnapshot {
        generation: h.app.generation,
        position_ms: 5100,
        seek_request_id: 17,
        ..Default::default()
    });
    h.app.update_playback().unwrap();
    assert_eq!(h.app.state.playback.seek_request_id, 17);
    assert_eq!(h.app.state.playback.position_ms, 5100);
    h.app.store.close().await;
}
