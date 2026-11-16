use anyhow::bail;
use mplayer_core::*;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::Instrument;

pub(super) struct LibraryService {
    pub(super) store: Store,
    pub(super) state: LibrarySnapshot,
    pub(super) probe: Arc<dyn MetadataProbe>,
    commands: mpsc::Receiver<CommandEnvelope<LibraryRequest>>,
    output: watch::Sender<LibrarySnapshot>,
    pub(super) scan_cancel: Option<ScanCancellationToken>,
    pub(super) scan_task: Option<JoinHandle<Result<()>>>,
    pub(super) scan_progress: Option<watch::Receiver<ScanProgress>>,
    pub(super) pending_imports: Vec<PathBuf>,
    trace: TraceContext,
    scan_traces: Vec<TraceContext>,
    pending_traces: Vec<TraceContext>,
}
impl LibraryService {
    pub(super) fn new(
        store: Store,
        probe: Arc<dyn MetadataProbe>,
        commands: mpsc::Receiver<CommandEnvelope<LibraryRequest>>,
        output: watch::Sender<LibrarySnapshot>,
    ) -> Self {
        Self {
            store,
            probe,
            commands,
            output,
            state: LibrarySnapshot::default(),
            scan_cancel: None,
            scan_task: None,
            scan_progress: None,
            pending_imports: Vec::new(),
            trace: TraceContext::new(&"library_start"),
            scan_traces: Vec::new(),
            pending_traces: Vec::new(),
        }
    }
    pub(super) async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        if let Err(error) = self.refresh().await {
            self.state.status = format!("Initialization failed: {error}");
        }
        self.publish();
        loop {
            tokio::select! {
                biased;
                _ = shutdown_requested(&mut shutdown) => break,
                command = self.commands.recv() => {
                    let Some(command) = command else { break };
                    self.trace = command.trace;
                    self.trace.event("library", "handling");
                    let span = self.trace.span.clone();
                    match command.payload {
                        LibraryRequest::Command(command) => {
                            match self.handle(command).instrument(span).await {
                                Ok(()) => self.trace.event("library", "handled"),
                                Err(error) => { self.trace.failed("library", &error); self.state.status = error.to_string(); }
                            }
                            self.publish();
                        }
                        LibraryRequest::Resolve { source, reply } => {
                            if reply.is_closed() { self.trace.event("library", "cancelled"); continue; }
                            let result = async {
                                let tracks = match source {
                                    TrackSource::Album(key) => self.store.album_tracks(&key).await?,
                                    TrackSource::Playlist(id) => self.store.playlist_entries(id).await?.into_iter().map(|entry| entry.track).collect(),
                                };
                                if tracks.is_empty() { bail!("Album or playlist has no available tracks"); }
                                Ok(tracks)
                            }.instrument(span).await;
                            match &result { Ok(_) => self.trace.event("library", "resolved"), Err(error) => self.trace.failed("library", error) }
                            let _ = reply.send(result);
                        }
                    }
                }
                result = async { match self.scan_task.as_mut() { Some(task) => Some(task.await), None => std::future::pending().await } } => {
                    self.scan_task = None;
                    self.finish_scan(result.expect("active scan")).await;
                    self.publish();
                }
                changed = async { match self.scan_progress.as_mut() { Some(progress) => progress.changed().await, None => std::future::pending().await } } => {
                    if changed.is_ok() {
                        let progress = self.scan_progress.as_mut().unwrap().borrow_and_update().clone();
                        let revision = self.state.scan.folder_revision;
                        self.state.scan = progress;
                        if revision != self.state.scan.folder_revision {
                            match self.store.folders(&self.state.folders.query, self.state.folders.offset).await {
                                Ok(folders) => self.state.folders = folders,
                                Err(error) => self.state.status = error.to_string(),
                            }
                        }
                        self.publish();
                    } else {
                        // Read the final value even if the sender closed before this select.
                        self.state.scan = self.scan_progress.take().unwrap().borrow().clone();
                    }
                }
            }
        }
        self.commands.close();
        while let Ok(command) = self.commands.try_recv() {
            command.trace.event("library", "cancelled");
        }
        for trace in self
            .pending_traces
            .drain(..)
            .chain(self.scan_traces.drain(..))
        {
            trace.event("library", "cancelled");
        }
        if let Some(cancel) = self.scan_cancel.take() {
            let _ = cancel.cancel();
        }
        if let Some(mut task) = self.scan_task.take()
            && tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
        {
            task.abort();
        }
        if let Some(progress) = self.scan_progress.take() {
            self.state.scan = progress.borrow().clone();
        }
        self.state.scan.running = false;
        self.publish();
        Ok(())
    }
    fn publish(&self) {
        publish_changed(&self.output, &self.state);
    }
    async fn finish_scan(
        &mut self,
        result: std::result::Result<Result<()>, tokio::task::JoinError>,
    ) {
        if let Some(progress) = self.scan_progress.take() {
            self.state.scan = progress.borrow().clone();
        }
        self.state.scan.running = false;
        let stage = match &result {
            Ok(Ok(())) => "scan_complete",
            Ok(Err(_)) if self.scan_cancel.is_none() => "scan_cancelled",
            _ => "scan_failed",
        };
        self.state.status = match result {
            Ok(Ok(())) => format!(
                "Scan complete: {} files, {} errors",
                self.state.scan.processed, self.state.scan.errors
            ),
            Ok(Err(_)) if self.scan_cancel.is_none() => "Scan cancelled".into(),
            Ok(Err(error)) => format!("Scan failed: {error}"),
            Err(error) => format!("Scan task failed: {error}"),
        };
        for trace in self.scan_traces.drain(..) {
            trace.event("library", stage);
            if stage == "scan_failed" {
                trace.failed("library", &self.state.status);
            }
        }
        self.scan_cancel = None;
        if let Err(error) = self.refresh().await {
            self.state.status = error.to_string();
        }
        if !self.pending_imports.is_empty() {
            let paths = std::mem::take(&mut self.pending_imports);
            self.state.queued_imports = 0;
            let traces = std::mem::take(&mut self.pending_traces);
            if let Some(trace) = traces.last() {
                self.trace = trace.clone();
            }
            for trace in &traces {
                trace.span.in_scope(|| tracing::debug!(target: "mplayer::commands", trace_id = %trace.id, batch_trace_id = %self.trace.id, component = "library", stage = "queued_import_started"));
            }
            let span = self.trace.span.clone();
            match self.start_scan(paths).instrument(span).await {
                Ok(()) if !traces.is_empty() => self.scan_traces = traces,
                Ok(()) => (),
                Err(error) => {
                    for trace in traces {
                        trace.failed("library", &error);
                    }
                    self.state.status = error.to_string();
                }
            }
        }
    }
    #[tracing::instrument(level = "debug", skip_all, err)]
    async fn refresh(&mut self) -> Result<()> {
        let (library, folders, albums) = tokio::try_join!(
            self.store.library_filtered(
                &self.state.library.query,
                self.state.library.offset,
                &self.state.library_filter
            ),
            self.store
                .folders(&self.state.folders.query, self.state.folders.offset),
            self.store
                .albums(&self.state.albums.query, self.state.albums.offset),
        )?;
        self.state.library = library;
        self.state.folders = folders;
        self.state.albums = albums;
        self.state.roots = self.store.roots().await?;
        self.state.playlists = self.store.playlists().await?;
        if let Some(id) = self.state.selected_playlist {
            self.state.playlist_entries = self.store.playlist_entries(id).await?.into();
        }
        Ok(())
    }

    pub(super) async fn handle(&mut self, command: LibraryCommand) -> Result<()> {
        match command {
            LibraryCommand::Import(paths) => self.import(paths).await?,
            LibraryCommand::RemoveScanFolder(path) => {
                if self.scan_task.is_some() || !self.pending_imports.is_empty() {
                    bail!("Wait for scanning to finish before removing a music folder");
                }
                self.store.remove_scan_folder(&path).await?;
                self.refresh().await?;
                self.state.status = "Music folder removed; files kept on disk".into();
            }
            LibraryCommand::Rescan => self.start_scan_request(ScanRequest::Library).await?,
            LibraryCommand::RescanFolder(id) => {
                self.start_scan_request(ScanRequest::Folder(id)).await?
            }
            LibraryCommand::CancelScan => {
                if let Some(cancel) = self.scan_cancel.take() {
                    let _ = cancel.cancel();
                }
                self.pending_imports.clear();
                for trace in self.pending_traces.drain(..) {
                    trace.event("library", "cancelled");
                }
                self.state.queued_imports = 0;
                self.state.status = "Cancelling scans…".into();
            }
            LibraryCommand::Search { query, offset } => {
                self.state.library = self
                    .store
                    .library_filtered(&query, offset, &self.state.library_filter)
                    .await?;
            }
            LibraryCommand::BrowseLibrary {
                filter,
                query,
                offset,
            } => {
                self.state.library = self.store.library_filtered(&query, offset, &filter).await?;
                self.state.library_filter = filter;
            }
            LibraryCommand::ListFolders { query, offset } => {
                self.state.folders = self.store.folders(&query, offset).await?;
            }
            LibraryCommand::ListAlbums { query, offset } => {
                self.state.albums = self.store.albums(&query, offset).await?;
            }
            LibraryCommand::CreatePlaylist(name) => {
                self.state.selected_playlist = Some(self.store.create_playlist(&name).await?);
                self.refresh().await?;
            }
            LibraryCommand::DeletePlaylist(id) => {
                self.store.delete_playlist(id).await?;
                if self.state.selected_playlist == Some(id) {
                    self.state.selected_playlist = None;
                    self.state.playlist_entries = Arc::default();
                }
                self.refresh().await?;
            }
            LibraryCommand::SelectPlaylist(id) => {
                self.state.playlist_entries = match id {
                    Some(id) => self.store.playlist_entries(id).await?.into(),
                    None => Arc::default(),
                };
                self.state.selected_playlist = id;
            }
            LibraryCommand::AddToPlaylist {
                playlist_id,
                track_id,
            } => {
                self.store.add_playlist_entry(playlist_id, track_id).await?;
                self.refresh().await?;
            }
            LibraryCommand::RemovePlaylistEntry(id) => {
                self.store.remove_playlist_entry(id).await?;
                self.refresh().await?;
            }
            LibraryCommand::MovePlaylistEntry { entry_id, position } => {
                self.store.move_playlist_entry(entry_id, position).await?;
                self.refresh().await?;
            }
        }
        Ok(())
    }
    #[tracing::instrument(skip_all, fields(input_count = paths.len()), err)]
    async fn import(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        if paths.is_empty() {
            bail!("Choose a music folder or local file first");
        }
        if self.scan_task.is_none() {
            return self.start_scan(paths).await;
        }
        let mut pending = self.pending_imports.clone();
        for path in paths {
            if !pending.contains(&path) {
                if pending.len() >= 64 {
                    bail!(
                        "The scan queue is full; wait for it to finish before adding more folders"
                    );
                }
                pending.push(path);
            }
        }
        self.state.queued_imports = pending.len();
        self.state.status = format!("{} path(s) queued for scanning", pending.len());
        // Each retained origin adds at least one path, bounding this list by
        // the same 64-path limit as the import queue.
        if self.pending_imports.len() != pending.len() {
            self.pending_traces.push(self.trace.clone());
        }
        self.pending_imports = pending;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(input_count = paths.len()), err)]
    pub(super) async fn start_scan(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        if paths.is_empty() {
            bail!("Add a music folder or local file first");
        }
        self.start_scan_request(ScanRequest::Roots(paths)).await
    }

    #[tracing::instrument(skip_all, fields(?request), err)]
    async fn start_scan_request(&mut self, request: ScanRequest) -> Result<()> {
        if self.scan_task.is_some() {
            bail!("A scan is already running; cancel it or wait for completion");
        }
        let progress = ScanProgress {
            running: true,
            ..Default::default()
        };
        let (sender, receiver) = watch::channel(progress);
        let store = self.store.clone();
        let probe = Arc::clone(&self.probe);
        let (cancel, task) = scan_task(store, request, probe, sender).await?;
        self.scan_cancel = Some(cancel);
        self.scan_traces = vec![self.trace.clone()];
        self.scan_task = Some(task);
        self.state.scan = receiver.borrow().clone();
        self.scan_progress = Some(receiver);
        self.state.status = "Scanning music…".into();
        Ok(())
    }
}
