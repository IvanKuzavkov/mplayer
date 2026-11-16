use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use eframe::egui;
use mplayer_core::ArtworkClient;
use tokio::sync::oneshot;

const MAX_CACHED: usize = 128;
const MAX_PENDING: usize = 2;
const THUMBNAIL_SIZE: u32 = 256;
type ImageResult = Result<Option<egui::ColorImage>, String>;

struct Entry {
    texture: Option<egui::TextureHandle>,
    used: u64,
}

struct Pending {
    path: PathBuf,
    generation: u64,
    result: oneshot::Receiver<ImageResult>,
}

#[derive(Default)]
pub(super) struct Thumbnails {
    pub client: Option<ArtworkClient>,
    entries: HashMap<PathBuf, Entry>,
    pending: Vec<Pending>,
    visible: Vec<PathBuf>,
    frame: u64,
    generation: u64,
}

impl Thumbnails {
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.entries.clear();
    }

    pub fn begin_frame(&mut self, context: &egui::Context) {
        self.frame = self.frame.wrapping_add(1);
        self.visible.clear();
        for index in (0..self.pending.len()).rev() {
            let result = match self.pending[index].result.try_recv() {
                Ok(result) => result,
                Err(oneshot::error::TryRecvError::Empty) => continue,
                Err(oneshot::error::TryRecvError::Closed) => Err("Thumbnail worker stopped".into()),
            };
            let pending = self.pending.swap_remove(index);
            if pending.generation != self.generation {
                continue;
            }
            let texture = match result {
                Ok(image) => image.map(|image| {
                    context.load_texture("library-cover", image, egui::TextureOptions::LINEAR)
                }),
                Err(error) => {
                    tracing::debug!(path = %pending.path.display(), %error, "Could not load library artwork");
                    None
                }
            };
            self.entries.insert(
                pending.path,
                Entry {
                    texture,
                    used: self.frame,
                },
            );
        }
    }

    pub fn texture(&mut self, path: &Path) -> Option<&egui::TextureHandle> {
        if !self.visible.iter().any(|visible| visible == path) {
            self.visible.push(path.to_owned());
        }
        let entry = self.entries.get_mut(path)?;
        entry.used = self.frame;
        entry.texture.as_ref()
    }

    pub fn finish_frame(&mut self, context: &egui::Context) {
        while self.entries.len() > MAX_CACHED {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(path, _)| path.clone())
                .expect("nonempty cache");
            self.entries.remove(&oldest);
        }
        let Some(client) = &self.client else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        for path in &self.visible {
            if self.pending.len() >= MAX_PENDING {
                break;
            }
            if self.entries.contains_key(path)
                || self.pending.iter().any(|pending| &pending.path == path)
            {
                continue;
            }
            let path = path.clone();
            let worker_path = path.clone();
            let client = client.clone();
            let wake = context.clone();
            let (sender, result) = oneshot::channel();
            runtime.spawn(async move {
                let result = match client.read(worker_path).await {
                    Ok(Some(artwork)) => tokio::task::spawn_blocking(move || {
                        super::artwork::decode_with_size(&artwork.data, THUMBNAIL_SIZE)
                    })
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result)
                    .map(Some),
                    Ok(None) => Ok(None),
                    Err(error) => Err(error.to_string()),
                };
                let _ = sender.send(result);
                wake.request_repaint();
            });
            self.pending.push(Pending {
                path,
                generation: self.generation,
                result,
            });
        }
        if !self.pending.is_empty() {
            context.request_repaint_after(Duration::from_millis(33));
        }
    }
}
