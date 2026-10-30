use crate::*;
use mplayer_core::AlbumKey;

impl PlayerApp {
    pub(super) fn album_artwork_actions(
        &mut self,
        ui: &mut egui::Ui,
        artwork: egui::Rect,
        key: &AlbumKey,
        has_tracks: bool,
    ) -> bool {
        #[cfg(test)]
        self.album_artworks.push(artwork);
        if !ui.rect_contains_pointer(artwork) {
            return false;
        }
        let bar = egui::Rect::from_center_size(
            egui::pos2(artwork.center().x, artwork.bottom() - 26.0),
            egui::vec2(76.0, 40.0),
        );
        ui.painter()
            .rect_filled(bar, 6.0, ui.visuals().window_fill());
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("album_actions", &key.title, &key.artist))
                .max_rect(bar.shrink(6.0))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        buttons.spacing_mut().item_spacing.x = 8.0;
        let enabled = self.available() && has_tracks;
        let enqueue = icon_buttons::icon_buttons(
            &mut buttons,
            icon_buttons::Icon::Queue,
            enabled,
            "Enqueue album",
        );
        let play = icon_buttons::icon_buttons(
            &mut buttons,
            icon_buttons::Icon::PlaySmall,
            enabled,
            "Play album",
        );
        #[cfg(test)]
        self.album_action_buttons.push([enqueue.rect, play.rect]);
        if enqueue.clicked() {
            self.submit(AppCommand::EnqueueAlbum(key.clone()));
        }
        if play.clicked() {
            self.submit(AppCommand::PlayAlbum(key.clone()));
        }
        ui.rect_contains_pointer(bar)
    }
}
