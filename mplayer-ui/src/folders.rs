//! Native folder dialogs, opened on the UI thread and awaited in the background.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use eframe::egui;
use tokio::{runtime::Handle, sync::oneshot, task::JoinHandle};
use tracing::Instrument;

type Selection = Option<Vec<PathBuf>>;

struct DialogLog {
    opened_at: Instant,
    outcome: &'static str,
    selected_count: usize,
}

impl DialogLog {
    fn opened() -> Self {
        tracing::info!("folder dialog opened");
        Self {
            opened_at: Instant::now(),
            outcome: "aborted",
            selected_count: 0,
        }
    }
}

impl Drop for DialogLog {
    fn drop(&mut self) {
        tracing::info!(
            outcome = self.outcome,
            selected_count = self.selected_count,
            elapsed_ms = self.opened_at.elapsed().as_millis(),
            "folder dialog closed"
        );
    }
}

#[derive(Default)]
pub(crate) struct FolderPicker {
    requested: Option<Option<PathBuf>>,
    pending: Option<oneshot::Receiver<Selection>>,
    task: Option<JoinHandle<()>>,
    ready: Option<Vec<PathBuf>>,
    error: Option<String>,
}

impl FolderPicker {
    pub(crate) fn is_open(&self) -> bool {
        self.requested.is_some() || self.pending.is_some() || self.ready.is_some()
    }

    pub(crate) fn open(&mut self, initial: Option<PathBuf>, _context: &egui::Context) {
        if !self.is_open() {
            self.error = None;
            self.requested = Some(initial);
        }
    }

    /// Called by eframe on its main thread, after rendering has recorded an open request.
    pub(crate) fn launch(&mut self, parent: &eframe::Frame, context: &egui::Context) {
        let Some(initial) = self.requested.take() else {
            return;
        };
        let runtime = match Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::warn!(%error, "folder dialog could not be opened");
                self.error = Some(format!("Cannot open the folder dialog: {error}"));
                return;
            }
        };
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title("Add music folders")
            .set_parent(parent);
        if let Some(directory) = initial {
            dialog = dialog.set_directory(directory);
        }
        // Construct the native dialog on the main thread (especially important for AppKit).
        let future = dialog.pick_folders();
        let dialog_log = DialogLog::opened();
        let (sender, receiver) = oneshot::channel();
        let repaint = context.clone();
        self.task = Some(
            runtime.spawn(
                async move {
                    let mut dialog_log = dialog_log;
                    let folders = future.await.map(|folders| {
                        folders
                            .into_iter()
                            .map(|folder| folder.path().to_owned())
                            .collect()
                    });
                    dialog_log.outcome = if folders.is_some() {
                        "selected"
                    } else {
                        "cancelled"
                    };
                    dialog_log.selected_count = folders.as_ref().map_or(0, Vec::len);
                    let _ = sender.send(folders);
                    repaint.request_repaint();
                }
                .in_current_span(),
            ),
        );
        self.pending = Some(receiver);
        context.request_repaint_after(Duration::from_millis(50));
    }

    /// Retain a selection until the application's bounded command queue accepts it.
    pub(crate) fn show(
        &mut self,
        context: &egui::Context,
        can_submit: bool,
    ) -> Option<Vec<PathBuf>> {
        if let Some(receiver) = &mut self.pending {
            match receiver.try_recv() {
                Ok(selection) => {
                    self.pending = None;
                    self.task = None;
                    self.ready = selection.filter(|folders| !folders.is_empty());
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    context.request_repaint_after(Duration::from_millis(50));
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.pending = None;
                    self.task = None;
                    self.error = Some(
                        "The system folder dialog stopped unexpectedly. Please try again.".into(),
                    );
                }
            }
        }
        if can_submit { self.ready.clone() } else { None }
    }

    pub(crate) fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    #[cfg(test)]
    pub(crate) fn complete_for_test(&mut self, folders: Vec<PathBuf>) {
        self.requested = None;
        self.ready = Some(folders);
    }

    pub(crate) fn close(&mut self) {
        self.requested = None;
        self.pending = None;
        self.ready = None;
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
impl Drop for FolderPicker {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[path = "../tests/unit/folders.rs"]
mod tests;
