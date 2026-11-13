use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use dirwalk::{Sort, WalkBuilder};
use tokio::{
    sync::watch,
    task::{JoinHandle, JoinSet},
};
use tracing::Instrument;

use crate::{
    MetadataProbe, Result, ScanProgress, ScanRequest, ScanStatus, Store,
    storage::{FolderRecord, MediaRecord, PreparedMedia, RootRecord},
};

mod cue;
const PROBE_CONCURRENCY: usize = 16;

pub struct ScanCancellationToken(tokio::sync::oneshot::Sender<()>);
impl ScanCancellationToken {
    pub fn cancel(self) -> Result<()> {
        self.0
            .send(())
            .map_err(|_| anyhow::anyhow!("scan cancellation receiver has closed"))
    }
}

struct InventoryFile {
    path: PathBuf,
    modified_secs: i64,
    file_size: u64,
}
struct Scanner {
    store: Store,
    probe: Arc<dyn MetadataProbe>,
    progress: watch::Sender<ScanProgress>,
    state: ScanProgress,
}

impl Scanner {
    fn supported_extensions<'a>() -> &'a [&'a str] {
        &[
            "flac", "wav", "wave", "mp3", "ogg", "oga", "dsf", "dff", "cue",
        ]
    }

    fn publish(&self) {
        self.progress.send_replace(self.state.clone());
    }
    fn folder_changed(&mut self) {
        self.state.folder_revision += 1;
        self.publish();
    }

    async fn run(&mut self, request: ScanRequest) -> Result<()> {
        if let ScanRequest::Folder(id) = request {
            let folder = self.store.folder(id).await?;
            let result = self.read_folder(&folder.path).await;
            match result {
                Ok(files) => self.process_folder(folder, files).await?,
                Err(error) => self.folder_read_error(&folder, error).await?,
            }
            return Ok(());
        }
        if let ScanRequest::Roots(paths) = request {
            let mut normalized = Vec::new();
            let mut file_folders = HashSet::new();
            let mut recursive = HashSet::new();
            for path in paths {
                // Saved offline roots must keep their identity even when canonicalization fails.
                match tokio::fs::canonicalize(&path).await {
                    Ok(path) => {
                        let path = if tokio::fs::metadata(&path).await?.is_file() {
                            let folder = path
                                .parent()
                                .context("file has no parent folder")?
                                .to_path_buf();
                            file_folders.insert(folder.clone());
                            folder
                        } else {
                            recursive.insert(path.clone());
                            path
                        };
                        normalized.push(path);
                    }
                    Err(error) => {
                        if self
                            .store
                            .scan_roots()
                            .await?
                            .iter()
                            .any(|root| root.path == path)
                        {
                            normalized.push(path);
                        } else {
                            tracing::warn!(path = %path.display(), %error, "cannot access import path");
                            self.state.errors += 1;
                        }
                    }
                }
            }
            normalized.sort();
            normalized.dedup();
            normalized.retain(|path| {
                !recursive
                    .iter()
                    .any(|parent| path != parent && path.starts_with(parent))
            });
            let mut selected = HashSet::new();
            let mut targeted = Vec::new();
            for path in normalized {
                let root = self.store.register_root(&path).await?;
                if file_folders.contains(&path) && !recursive.contains(&path) {
                    targeted.push((root, path));
                } else {
                    selected.insert(root.id);
                }
            }
            for root in self
                .store
                .scan_roots()
                .await?
                .into_iter()
                .filter(|root| selected.contains(&root.id))
            {
                self.process_root(root).await?;
            }
            for (root, path) in targeted {
                if selected.contains(&root.id) {
                    continue;
                }
                let folder = self.store.ensure_folder(&root, &path).await?;
                match self.read_folder(&path).await {
                    Ok(files) => self.process_folder(folder, files).await?,
                    Err(error) => self.folder_read_error(&folder, error).await?,
                }
            }
        } else {
            for root in self.store.scan_roots().await? {
                self.process_root(root).await?;
            }
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(root = %root.path.display()), err)]
    async fn process_root(&mut self, mut root: RootRecord) -> Result<()> {
        root.generation = self.store.begin_root_scan(root.id).await?;
        let path = root.path.clone();
        let span = tracing::Span::current();
        let walk = tokio::task::spawn_blocking(move || {
            span.in_scope(|| {
                WalkBuilder::new(path)
                    .extensions(Scanner::supported_extensions())
                    .sort(Sort::Name)
                    .build()
            })
        })
        .await?;
        let walk = match walk {
            Ok(walk) => walk,
            Err(error) => {
                self.state.errors += 1;
                self.store
                    .root_status(root.id, ScanStatus::Error, Some(&error.to_string()))
                    .await?;
                for folder in self.store.root_folders(root.id).await? {
                    self.store
                        .folder_status(folder.id, ScanStatus::Error, Some(&error.to_string()))
                        .await?;
                }
                self.folder_changed();
                return Ok(());
            }
        };
        let complete = walk.errors.is_empty();
        let mut blocked = Vec::new();
        for error in &walk.errors {
            let dirwalk::Error::Io { path, .. } = error;
            let path = if path.is_absolute() {
                path.clone()
            } else {
                root.path.join(path)
            };
            // An error may refer to a directory or a file. Protect both possible inventories.
            blocked.push(path.parent().unwrap_or(&path).to_path_buf());
            tracing::warn!(%error, "folder discovery failed");
        }
        if !complete {
            self.state.errors += walk.errors.len() as u64;
            let error = walk
                .errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            self.store
                .root_status(root.id, ScanStatus::Error, Some(&error))
                .await?;
        }
        let mut directories = HashSet::from([root.path.clone()]);
        let mut inventories: BTreeMap<PathBuf, Vec<InventoryFile>> = BTreeMap::new();
        for entry in walk.entries {
            if entry.is_symlink {
                continue;
            }
            let path = root.path.join(entry.relative_path);
            if entry.is_dir {
                directories.insert(path);
                continue;
            }
            inventories
                .entry(path.parent().context("media has no parent")?.into())
                .or_default()
                .push(InventoryFile {
                    path,
                    modified_secs: entry.modified,
                    file_size: entry.size,
                });
        }
        let existing = self.store.root_folders(root.id).await?;
        for folder in &existing {
            if directories.contains(&folder.path) {
                inventories.entry(folder.path.clone()).or_default();
            }
        }
        let before_errors = self.state.errors;
        for (path, files) in inventories {
            let folder = self.store.ensure_folder(&root, &path).await?;
            if blocked.iter().any(|blocked| path.starts_with(blocked)) {
                // Re-enumerate only ambiguous/error scopes. A failed sibling must not
                // prevent this folder from committing a proven complete inventory.
                match self.read_folder(&path).await {
                    Ok(files) => self.process_folder(folder, files).await?,
                    Err(error) => self.folder_read_error(&folder, error).await?,
                }
            } else {
                self.process_folder(folder, files).await?;
            }
        }
        if complete {
            for folder in &existing {
                if !directories.contains(&folder.path) {
                    self.store.delete_folder(folder.id).await?;
                    self.folder_changed();
                }
            }
            let status = if self.state.errors == before_errors {
                ScanStatus::Ready
            } else {
                ScanStatus::Error
            };
            self.store
                .root_status(
                    root.id,
                    status,
                    (status == ScanStatus::Error)
                        .then_some("Some folders failed; see folder errors"),
                )
                .await?;
        }
        if !complete {
            for folder in existing {
                if !directories.contains(&folder.path) {
                    self.folder_error(
                        folder.id,
                        anyhow::anyhow!(
                            "Root discovery was incomplete; folder availability is unknown"
                        ),
                    )
                    .await?;
                }
            }
        }
        self.folder_changed();
        Ok(())
    }

    async fn read_folder(&self, path: &Path) -> Result<Vec<InventoryFile>> {
        let span = tracing::Span::current();
        let cloned_path = path.to_path_buf();
        let entries = tokio::task::spawn_blocking(move || {
            span.in_scope(|| {
                WalkBuilder::new(cloned_path)
                    .max_depth(1)
                    .extensions(Scanner::supported_extensions())
                    .sort(Sort::Name)
                    .build()
            })
        })
        .await?
        .map_err(|e| match e {
            dirwalk::Error::Io { path: _, source } => source,
        })?;
        let mut files = Vec::new();
        for entry in entries.entries {
            if entry.is_dir || entry.is_symlink {
                continue;
            }

            files.push(InventoryFile {
                path: path.join(entry.relative_path),
                modified_secs: entry.modified,
                file_size: entry.size,
            });
        }
        Ok(files)
    }

    async fn folder_read_error(
        &mut self,
        folder: &FolderRecord,
        error: anyhow::Error,
    ) -> Result<()> {
        // NotFound may refer to a file that vanished while enumerating the folder.
        // Confirm the folder itself is gone and its scan root is still online.
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            && tokio::fs::metadata(&folder.path)
                .await
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            for root in self.store.scan_roots().await? {
                if folder.path.starts_with(&root.path)
                    && tokio::fs::metadata(&root.path)
                        .await
                        .is_ok_and(|metadata| metadata.is_dir())
                {
                    self.store.delete_folder(folder.id).await?;
                    self.folder_changed();
                    return Ok(());
                }
            }
        }
        self.folder_error(folder.id, error).await
    }

    async fn folder_error(&mut self, id: i64, error: anyhow::Error) -> Result<()> {
        tracing::warn!(folder_id = id, %error, "folder scan failed; previous contents retained");
        self.state.errors += 1;
        self.store
            .folder_status(id, ScanStatus::Error, Some(&format!("{error:#}")))
            .await?;
        self.folder_changed();
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(folder = %folder.path.display(), generation = folder.generation), err)]
    async fn process_folder(
        &mut self,
        folder: FolderRecord,
        files: Vec<InventoryFile>,
    ) -> Result<()> {
        self.store
            .folder_status(folder.id, ScanStatus::Scanning, None)
            .await?;
        self.folder_changed();
        let result = async {
            let media = self.prepare_folder(&folder, files).await?;
            self.store.commit_folder(&folder, media).await
        }
        .await;
        match result {
            Ok(()) => self.folder_changed(),
            Err(error) => self.folder_error(folder.id, error).await?,
        }
        Ok(())
    }

    async fn prepare_folder(
        &mut self,
        folder: &FolderRecord,
        files: Vec<InventoryFile>,
    ) -> Result<Vec<PreparedMedia>> {
        let mut cached: HashMap<_, _> = self
            .store
            .folder_snapshot(folder.id)
            .await?
            .into_iter()
            .map(|media| (media.path.clone(), media))
            .collect();
        let mut jobs = JoinSet::new();
        let mut media = Vec::new();
        let mut cues = Vec::new();
        self.state.discovered += files.iter().filter(|file| !is_cue(&file.path)).count() as u64;
        for file in files {
            if is_cue(&file.path) {
                cues.push(file.path);
                continue;
            }
            if let Some(cached) = cached.remove(&file.path).filter(|cached| {
                (cached.modified_secs, cached.file_size) == (file.modified_secs, file.file_size)
            }) {
                media.push(PreparedMedia::whole(cached));
                self.state.processed += 1;
            } else {
                let probe = self.probe.clone();
                let span = tracing::Span::current();
                jobs.spawn_blocking(move || {
                    span.in_scope(|| -> Result<PreparedMedia> {
                        let metadata = probe
                            .probe(&file.path)
                            .with_context(|| format!("Cannot read {}", file.path.display()))?;
                        Ok(PreparedMedia::whole(MediaRecord {
                            path: file.path,
                            modified_secs: file.modified_secs,
                            file_size: file.file_size,
                            metadata,
                        }))
                    })
                });
            }
            if jobs.len() >= PROBE_CONCURRENCY {
                self.collect_job(&mut jobs, &mut media).await?;
            }
            self.publish();
        }
        while !jobs.is_empty() {
            self.collect_job(&mut jobs, &mut media).await?;
        }
        for path in cues {
            let contents = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("Cannot read {}", path.display()))?;
            cue::apply(&path, &contents, &mut media)
                .await
                .with_context(|| format!("Cannot process CUE {}", path.display()))?;
        }
        Ok(media)
    }

    async fn collect_job(
        &mut self,
        jobs: &mut JoinSet<Result<PreparedMedia>>,
        media: &mut Vec<PreparedMedia>,
    ) -> Result<()> {
        if let Some(result) = jobs.join_next().await {
            self.state.processed += 1;
            media.push(result??);
            self.publish();
        }
        Ok(())
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.state.running = false;
        self.publish();
    }
}

fn is_cue(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
}

#[tracing::instrument(skip_all, fields(?request), err)]
pub async fn scan_task(
    store: Store,
    request: ScanRequest,
    probe: Arc<dyn MetadataProbe>,
    progress: watch::Sender<ScanProgress>,
) -> Result<(ScanCancellationToken, JoinHandle<Result<()>>)> {
    let state = ScanProgress {
        running: true,
        ..Default::default()
    };
    progress.send_replace(state.clone());
    let mut scanner = Scanner {
        store,
        probe,
        progress,
        state,
    };
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(
        async move {
            let (result, cancelled) = tokio::select! {
                biased;
                _ = receiver => (Err(anyhow::anyhow!("scanner cancelled")), true),
                result = scanner.run(request) => (result, false),
            };
            if let Err(error) = &result {
                scanner
                    .store
                    .interrupt_scans(
                        if cancelled {
                            ScanStatus::Cancelled
                        } else {
                            ScanStatus::Error
                        },
                        &error.to_string(),
                    )
                    .await?;
                scanner.folder_changed();
            }
            tracing::info!(cancelled, errors = scanner.state.errors, "Scan finished");
            result
        }
        .in_current_span(),
    );
    Ok((ScanCancellationToken(sender), task))
}

#[cfg(test)]
#[path = "../tests/unit/scanner.rs"]
mod tests;
