use super::*;

struct PlayerRig {
    router: CommandRouter,
    library: mpsc::Receiver<CommandEnvelope<LibraryRequest>>,
    audio: Receiver<CommandEnvelope<AudioCommand>>,
    audio_state: watch::Sender<PlaybackSnapshot>,
    state: watch::Receiver<PlayerSnapshot>,
    _settings: watch::Receiver<SettingsUpdate>,
    _task: TaskGuard,
}
impl PlayerRig {
    async fn new() -> Self {
        let (audio, output) = sync_channel(64);
        let (audio_state, playback) = watch::channel(PlaybackSnapshot::default());
        let (sender, inbox) = mpsc::channel(64);
        let (catalog, library) = mpsc::channel(64);
        let client = LibraryClient(catalog);
        let (output_state, state) = watch::channel(PlayerSnapshot::default());
        let trace = TraceContext::new(&"initial_settings");
        let (desired, settings) = watch::channel(SettingsUpdate {
            volume: trace.wrap(1.0),
            device: trace.wrap(None),
        });
        let (stop, shutdown) = watch::channel(false);
        let player = PlaybackService::new(
            audio,
            playback,
            inbox,
            output_state,
            client.clone(),
            desired,
        );
        let task = TaskGuard(tokio::spawn(player.run(shutdown)));
        let rig = Self {
            router: CommandRouter::new(sender, client, stop),
            library,
            audio: output,
            audio_state,
            state,
            _settings: settings,
            _task: task,
        };
        rig.audio(|command| matches!(command, AudioCommand::SetDevice(_)))
            .await;
        rig
    }
    fn send(&self, command: PlaybackCommand) -> TraceId {
        let command = self.router.prepare(command);
        let id = command.trace.id;
        self.router.try_send(command).unwrap();
        id
    }
    async fn audio(
        &self,
        predicate: impl Fn(&AudioCommand) -> bool,
    ) -> CommandEnvelope<AudioCommand> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(command) = self.audio.try_recv()
                    && predicate(&command.payload)
                {
                    return command;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("audio command was blocked")
    }
    async fn resolve(&mut self) -> CommandEnvelope<LibraryRequest> {
        tokio::time::timeout(Duration::from_secs(3), self.library.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn playing(&mut self) {
        self.send(PlaybackCommand::Play(track(1)));
        let command = self
            .audio(|command| matches!(command, AudioCommand::Load { .. }))
            .await;
        let AudioCommand::Load { generation, .. } = command.payload else {
            unreachable!()
        };
        self.audio_state.send_replace(PlaybackSnapshot {
            generation,
            state: PlaybackState::Playing,
            track: Some(track(1)),
            ..Default::default()
        });
        wait_for(&mut self.state, |state| {
            state.playback.state == PlaybackState::Playing
        })
        .await;
    }
}

#[tokio::test]
async fn slow_lookup_does_not_block_controls_and_trace_reaches_audio() {
    let mut rig = PlayerRig::new().await;
    rig.playing().await;
    let lookup_id = rig.send(PlaybackCommand::PlayAlbum(AlbumKey::default()));
    let lookup = rig.resolve().await;
    assert_eq!(lookup.trace.id, lookup_id);
    let LibraryRequest::Resolve { reply, .. } = lookup.payload else {
        unreachable!()
    };
    for (command, expected) in [
        (PlaybackCommand::SetVolume(0.25), "volume"),
        (PlaybackCommand::PauseResume, "pause"),
        (
            PlaybackCommand::Seek {
                position_ms: 123,
                request_id: 7,
            },
            "seek",
        ),
    ] {
        let id = rig.send(command);
        let received = rig
            .audio(|command| {
                matches!(
                    (command, expected),
                    (AudioCommand::SetVolume(0.25), "volume")
                        | (AudioCommand::Pause, "pause")
                        | (
                            AudioCommand::Seek {
                                position_ms: 123,
                                request_id: 7
                            },
                            "seek"
                        )
                )
            })
            .await;
        assert_eq!(received.trace.id, id);
    }
    let stop = rig.send(PlaybackCommand::Stop);
    assert_eq!(
        rig.audio(|command| matches!(command, AudioCommand::Stop { .. }))
            .await
            .trace
            .id,
        stop
    );
    wait_for(&mut rig.state, |state| {
        state.playback.state == PlaybackState::Stopped
    })
    .await;
    assert!(reply.send(Ok(vec![track(99)])).is_err());
    assert!(rig.audio.try_recv().is_err());
}

#[tokio::test]
async fn clear_and_new_play_invalidate_pending_lookup_results() {
    for replacement in [PlaybackCommand::ClearQueue, PlaybackCommand::Play(track(7))] {
        let mut rig = PlayerRig::new().await;
        rig.playing().await;
        rig.send(PlaybackCommand::PlayAlbum(AlbumKey::default()));
        let LibraryRequest::Resolve { reply, .. } = rig.resolve().await.payload else {
            unreachable!()
        };
        let clear = matches!(replacement, PlaybackCommand::ClearQueue);
        let id = rig.send(replacement);
        let received = rig
            .audio(|command| {
                matches!(
                    command,
                    AudioCommand::Stop { .. } | AudioCommand::Load { .. }
                )
            })
            .await;
        assert_eq!(received.trace.id, id);
        wait_for(&mut rig.state, |state| {
            if clear {
                state.queue.is_empty()
            } else {
                state.queue.first().is_some_and(|track| track.id == 7)
            }
        })
        .await;
        assert!(reply.send(Ok(vec![track(99)])).is_err());
        assert!(rig.audio.try_recv().is_err());
    }
}

#[tokio::test]
async fn enqueue_order_is_preserved_behind_a_pending_queue_replacement() {
    let mut rig = PlayerRig::new().await;
    let play_id = rig.send(PlaybackCommand::PlayAlbum(AlbumKey::default()));
    let first = rig.resolve().await;
    let append_id = rig.send(PlaybackCommand::EnqueueAlbum(AlbumKey {
        title: "second".into(),
        ..Default::default()
    }));
    rig.send(PlaybackCommand::Enqueue(track(5)));
    let LibraryRequest::Resolve { reply, .. } = first.payload else {
        unreachable!()
    };
    reply.send(Ok(vec![track(1), track(2)])).unwrap();
    assert_eq!(
        rig.audio(|command| matches!(command, AudioCommand::Load { .. }))
            .await
            .trace
            .id,
        play_id
    );
    let second = rig.resolve().await;
    assert_eq!(second.trace.id, append_id);
    let LibraryRequest::Resolve { reply, .. } = second.payload else {
        unreachable!()
    };
    reply.send(Ok(vec![track(3), track(4)])).unwrap();
    wait_for(&mut rig.state, |state| state.queue.len() == 5).await;
    assert_eq!(
        rig.state
            .borrow()
            .queue
            .iter()
            .map(|track| track.id)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
}

#[tokio::test]
async fn shutdown_persists_preferences_already_accepted_into_the_inbox() {
    let directory = TempDirectory::new();
    let store = Store::open(&directory.0.join("shutdown.sqlite"))
        .await
        .unwrap();
    let (audio, _commands) = sync_channel(64);
    let (_engine, playback) = watch::channel(PlaybackSnapshot::default());
    let (writer, _guard) = tracing_appender::non_blocking(std::io::sink());
    let app = Application::start(
        store.clone(),
        audio,
        playback,
        Arc::new(Probe),
        writer.error_counter(),
    )
    .await
    .unwrap();
    app.commands
        .try_send(app.commands.prepare(PlaybackCommand::SetVolume(0.17)))
        .unwrap();
    app.commands
        .try_send(
            app.commands
                .prepare(PlaybackCommand::SetDevice(Some("DAC".into()))),
        )
        .unwrap();
    app.commands.shutdown();
    tokio::time::timeout(Duration::from_secs(3), app.run())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.get_setting("volume").await.unwrap().as_deref(),
        Some("0.17")
    );
    assert_eq!(
        store.get_setting("output_device").await.unwrap().as_deref(),
        Some("DAC")
    );
    store.close().await;
}

#[tokio::test]
async fn unchanged_player_state_stays_quiet_and_audio_failure_is_local() {
    let mut rig = PlayerRig::new().await;
    rig.state.borrow_and_update();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), rig.state.changed())
            .await
            .is_err()
    );
    let state = rig.state.clone();
    drop(rig.audio_state);
    let mut state = state;
    wait_for(&mut state, |state| {
        state.playback.error.as_deref() == Some("Audio engine has stopped")
    })
    .await;
    assert!(rig.router.playback_closed());
    assert!(!rig.router.library_closed());
    rig.router
        .try_send(rig.router.prepare(LibraryCommand::Rescan))
        .unwrap();
    assert!(matches!(
        rig.library.recv().await.unwrap().payload,
        LibraryRequest::Command(LibraryCommand::Rescan)
    ));
}

#[tokio::test]
async fn cover_reset_survives_coalesced_stop_and_replay_and_stale_worker_failure() {
    struct Probe {
        calls: std::sync::atomic::AtomicUsize,
        started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: std::sync::Mutex<Receiver<()>>,
        panic: bool,
    }
    impl MetadataProbe for Probe {
        fn probe(&self, _: &Path) -> Result<TrackMetadata> {
            Ok(TrackMetadata::default())
        }
        fn artwork(&self, _: &Path) -> Result<Option<Artwork>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == 1 {
                self.started
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                assert!(!self.panic, "simulated stale artwork worker panic");
            }
            Ok(Some(Artwork {
                data: vec![call as u8].into(),
                mime_type: "test/data".into(),
            }))
        }
    }
    for panic in [false, true] {
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = sync_channel(1);
        let probe = Arc::new(Probe {
            calls: Default::default(),
            started: std::sync::Mutex::new(Some(started)),
            release: std::sync::Mutex::new(wait),
            panic,
        });
        let mut cover = CoverHarness::new(probe.clone());
        cover.set(Some(track(1)));
        tokio::time::timeout(Duration::from_secs(3), ready)
            .await
            .unwrap()
            .unwrap();
        // A watch receiver may observe only the replay, but the reset revision remains.
        cover.player.send_modify(|state| {
            state.playback.track = None;
            state.artwork_revision += 1;
        });
        cover.set(Some(track(1)));
        wait_for(&mut cover.state, |state| state.revision == 1).await;
        assert!(cover.state.borrow().artwork.is_none());
        release.send(()).unwrap();
        cover.expect(Some(&[2])).await;
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    }
}
