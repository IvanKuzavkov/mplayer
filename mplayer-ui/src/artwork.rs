use eframe::egui;
use mplayer_core::Artwork;
use std::{io::Cursor, sync::Arc, time::Duration};
use tokio::sync::oneshot;

const MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 8192;
const MAX_DECODE_ALLOCATION: u64 = 64 * 1024 * 1024;
const MAX_TEXTURE_DIMENSION: u32 = 1024;

struct Pending {
    source: Arc<[u8]>,
    result: oneshot::Receiver<Result<egui::ColorImage, String>>,
}

#[derive(Default)]
pub(super) struct Cover {
    source: Option<Arc<[u8]>>,
    texture: Option<egui::TextureHandle>,
    completed: Option<Arc<[u8]>>,
    pending: Option<Pending>,
}
impl Cover {
    pub fn update(&mut self, artwork: Option<&Artwork>, context: &egui::Context) {
        let next = artwork.map(|art| Arc::clone(&art.data));
        if same_source(self.source.as_ref(), next.as_ref()) {
            // Adopt the new Arc after a content match so later frames use pointer equality.
            self.source = next;
            if self.completed.is_some() {
                self.completed = self.source.clone();
            }
        } else {
            self.source = next;
            // Keep the displayed cover until the replacement has decoded.
            if self.source.is_none() {
                self.texture = None;
            }
            self.completed = None;
        }
        self.poll(context);
        if self.pending.is_none()
            && let Some(source) = self.source.as_ref()
            && !same_source(Some(source), self.completed.as_ref())
        {
            self.start(Arc::clone(source), context);
        }
        if self.pending.is_some() {
            // A worker also wakes the UI on completion. This closes the race where
            // a repaint was requested immediately before a native viewport slept.
            context.request_repaint_after(Duration::from_millis(33));
        }
    }
    pub fn texture(&self) -> Option<&egui::TextureHandle> {
        self.texture.as_ref()
    }

    fn poll(&mut self, context: &egui::Context) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return,
            Err(oneshot::error::TryRecvError::Closed) => {
                Err("Artwork decoder worker stopped before returning a result".into())
            }
        };
        let pending = self.pending.take().expect("pending result was just polled");
        if !same_source(Some(&pending.source), self.source.as_ref()) {
            return;
        }
        self.completed = Some(pending.source);
        match result {
            Ok(image) => {
                // Egui texture creation stays on the UI thread; workers return CPU pixels.
                self.texture =
                    Some(context.load_texture("album-cover", image, egui::TextureOptions::LINEAR));
            }
            Err(error) => {
                self.texture = None;
                tracing::warn!(%error, "Could not decode embedded album artwork");
            }
        }
    }

    fn start(&mut self, source: Arc<[u8]>, context: &egui::Context) {
        if source.len() > MAX_ENCODED_BYTES {
            self.completed = Some(source);
            self.texture = None;
            tracing::warn!("Embedded album artwork exceeds the 8 MiB limit");
            return;
        }
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                self.completed = Some(source);
                self.texture = None;
                tracing::warn!(%error, "Album artwork decoding requires the application runtime");
                return;
            }
        };
        let (sender, result) = oneshot::channel();
        let worker_source = Arc::clone(&source);
        let wake = context.clone();
        let parent = tracing::Span::current();
        runtime.spawn_blocking(move || {
            parent.in_scope(|| {
                let result = decode(&worker_source);
                let _ = sender.send(result);
                wake.request_repaint();
            })
        });
        self.pending = Some(Pending { source, result });
    }
}

fn same_source(first: Option<&Arc<[u8]>>, second: Option<&Arc<[u8]>>) -> bool {
    match (first, second) {
        (Some(first), Some(second)) => Arc::ptr_eq(first, second) || first == second,
        (None, None) => true,
        _ => false,
    }
}

#[tracing::instrument(level = "debug", skip_all, fields(encoded_bytes = source.len()))]
fn decode(source: &[u8]) -> Result<egui::ColorImage, String> {
    decode_with_size(source, MAX_TEXTURE_DIMENSION)
}

pub(super) fn decode_with_size(
    source: &[u8],
    max_dimension: u32,
) -> Result<egui::ColorImage, String> {
    if source.len() > MAX_ENCODED_BYTES {
        return Err("Encoded artwork exceeds the 8 MiB limit".into());
    }
    let mut reader = image::ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Err("Only PNG and JPEG album artwork is supported".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOCATION);
    reader.limits(limits);
    let image = reader.decode().map_err(|error| error.to_string())?;
    if image.width() == 0 || image.height() == 0 {
        return Err("Artwork image is empty".into());
    }
    let thumbnail = if image.width().max(image.height()) > max_dimension {
        image.thumbnail(max_dimension, max_dimension)
    } else {
        image
    };
    let rgba = thumbnail.into_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

pub(super) fn paint_cover(
    ui: &egui::Ui,
    texture: Option<&egui::TextureHandle>,
    rect: egui::Rect,
    corner_radius: u8,
) {
    let Some(texture) = texture else {
        super::player::record_icon(ui.painter(), rect);
        return;
    };
    let aspect = texture.size_vec2().x / texture.size_vec2().y;
    let uv = if aspect > 1.0 {
        let inset = (1.0 - 1.0 / aspect) * 0.5;
        egui::Rect::from_min_max(egui::pos2(inset, 0.0), egui::pos2(1.0 - inset, 1.0))
    } else {
        let inset = (1.0 - aspect) * 0.5;
        egui::Rect::from_min_max(egui::pos2(0.0, inset), egui::pos2(1.0, 1.0 - inset))
    };
    egui::Image::from_texture((texture.id(), texture.size_vec2()))
        .uv(uv)
        .corner_radius(corner_radius)
        .paint_at(ui, rect);
}

#[cfg(test)]
#[path = "../tests/unit/artwork.rs"]
mod tests;
