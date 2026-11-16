use super::*;
use library::TrackAction;

#[derive(Clone, Copy)]
struct PlaylistDrag {
    playlist_id: i64,
    entry_id: i64,
}

impl PlayerApp {
    pub(super) fn playlists(&mut self, ui: &mut egui::Ui, state: &AppSnapshot) {
        let Some(playlist) = state
            .playlists
            .iter()
            .find(|item| Some(item.id) == state.selected_playlist)
        else {
            self.playlist_list(ui, state);
            return;
        };
        let back =
            icon_buttons::icon_buttons(ui, icon_buttons::Icon::Back, self.available(), "Back");
        #[cfg(test)]
        {
            self.back_button = Some(back.rect);
        }
        if back.clicked() {
            self.submit(LibraryCommand::SelectPlaylist(None));
        }
        ui.add_space(8.0);
        let (header, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
        icon_buttons::playlist_icon(
            ui.painter(),
            egui::Rect::from_min_size(header.min, egui::vec2(64.0, 64.0)),
            theme::accent(ui.visuals()),
        );
        let left = header.left() + 80.0;
        let mut text_ui = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(
            egui::pos2(left, header.top()),
            egui::pos2(header.right(), header.bottom() - 32.0),
        )));
        text_ui
            .add(egui::Label::new(RichText::new(&playlist.name).size(20.0).strong()).truncate())
            .on_hover_text(&playlist.name);
        let mut count_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(
                    egui::pos2(left, header.bottom() - 28.0),
                    egui::pos2(header.right() - 36.0, header.bottom()),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        count_ui.add(
            egui::Label::new(
                RichText::new(format!("{} tracks", state.playlist_entries.len()))
                    .size(15.0)
                    .color(ui.visuals().weak_text_color()),
            )
            .truncate(),
        );
        let mut button_ui =
            ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                header.right_bottom() - egui::vec2(36.0, 28.0),
                egui::vec2(28.0, 28.0),
            )));
        let play = icon_buttons::icon_buttons(
            &mut button_ui,
            icon_buttons::Icon::PlaySmall,
            self.playback_available()
                && self.available()
                && state
                    .playlist_entries
                    .iter()
                    .any(|entry| !entry.track.missing),
            "Play playlist",
        );
        #[cfg(test)]
        {
            self.playlist_play = Some(play.rect);
        }
        if play.clicked() {
            self.submit(PlaybackCommand::PlayPlaylist(playlist.id));
        }
        ui.add_space(10.0);
        ui.separator();
        if state.playlist_entries.is_empty() {
            ui.label(
                RichText::new("Add tracks from their context menu in the media library.")
                    .color(ui.visuals().weak_text_color()),
            );
        }
        egui::ScrollArea::vertical()
            .id_salt(("playlist_tracks", playlist.id))
            .auto_shrink([false, false])
            .show_rows(ui, 54.0, state.playlist_entries.len(), |ui, range| {
                // Keep virtualized rows reachable while dragging beyond the visible page.
                if let Some(drag) = egui::DragAndDrop::payload::<PlaylistDrag>(ui.ctx())
                    && drag.playlist_id == playlist.id
                    && let Some(pointer) = ui.input(|input| input.pointer.hover_pos())
                    && ui.clip_rect().contains(pointer)
                {
                    let delta = if pointer.y < ui.clip_rect().top() + 28.0 {
                        6.0
                    } else if pointer.y > ui.clip_rect().bottom() - 28.0 {
                        -6.0
                    } else {
                        0.0
                    };
                    if delta != 0.0 {
                        ui.scroll_with_delta(egui::vec2(0.0, delta));
                        ui.ctx().request_repaint();
                    }
                }
                for index in range {
                    let entry = &state.playlist_entries[index];
                    ui.push_id(entry.id, |ui| {
                        let response = self.track_row(
                            ui,
                            &entry.track,
                            state,
                            index,
                            TrackAction::RemovePlaylist(entry.id),
                            true,
                        );
                        #[cfg(test)]
                        self.playlist_rows.push(response.rect);
                        if self.available() {
                            response.dnd_set_drag_payload(PlaylistDrag {
                                playlist_id: playlist.id,
                                entry_id: entry.id,
                            });
                            if let Some(drag) = response.dnd_hover_payload::<PlaylistDrag>()
                                && drag.playlist_id == playlist.id
                                && let Some(source) = state
                                    .playlist_entries
                                    .iter()
                                    .position(|entry| entry.id == drag.entry_id)
                            {
                                let below = ui
                                    .input(|input| input.pointer.hover_pos())
                                    .is_some_and(|pos| pos.y >= response.rect.center().y);
                                let slot = index + usize::from(below);
                                let position = slot - usize::from(source < slot);
                                if position != source {
                                    let y = if below {
                                        response.rect.bottom()
                                    } else {
                                        response.rect.top()
                                    };
                                    ui.painter().hline(
                                        response.rect.x_range(),
                                        y,
                                        egui::Stroke::new(2.0, theme::accent(ui.visuals())),
                                    );
                                    if response.dnd_release_payload::<PlaylistDrag>().is_some() {
                                        self.submit(LibraryCommand::MovePlaylistEntry {
                                            entry_id: drag.entry_id,
                                            position,
                                        });
                                    }
                                }
                            }
                        }
                    });
                }
            });
    }

    fn playlist_list(&mut self, ui: &mut egui::Ui, state: &AppSnapshot) {
        if self
            .rejected_playlist_name
            .as_ref()
            .is_some_and(|name| name != &self.playlist_name)
        {
            self.rejected_playlist_name = None;
        }
        egui::Panel::bottom("create_playlist")
            .exact_size(76.0)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let (notice, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 26.0),
                    egui::Sense::hover(),
                );
                ui.horizontal(|ui| {
                    let entry = ui.add(
                        egui::TextEdit::singleline(&mut self.playlist_name)
                            .hint_text("New playlist name")
                            .desired_width((ui.available_width() - 38.0).max(80.0)),
                    );
                    if entry.changed() {
                        self.rejected_playlist_name = None;
                    }
                    let valid = self.available() && !self.playlist_name.trim().is_empty();
                    let create = icon_buttons::icon_buttons(
                        ui,
                        icon_buttons::Icon::Add,
                        valid,
                        "Create playlist",
                    );
                    #[cfg(test)]
                    {
                        self.playlist_create = Some(create.rect);
                    }
                    if valid && create.clicked() {
                        let name = self.playlist_name.trim();
                        if state
                            .playlists
                            .iter()
                            .any(|playlist| playlist.name.trim().eq_ignore_ascii_case(name))
                        {
                            self.rejected_playlist_name = Some(self.playlist_name.clone());
                        } else if self.submit(LibraryCommand::CreatePlaylist(name.into())) {
                            self.playlist_name.clear();
                            self.rejected_playlist_name = None;
                        }
                    }
                });
                if self.rejected_playlist_name.is_some() {
                    ui.put(
                        notice,
                        egui::Label::new(
                            RichText::new("A playlist with this name already exists.")
                                .small()
                                .color(ui.visuals().error_fg_color),
                        )
                        .wrap(),
                    );
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("playlists")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if state.playlists.is_empty() {
                            ui.label(
                                RichText::new(
                                    "Create a playlist to collect your favourite tracks.",
                                )
                                .color(ui.visuals().weak_text_color()),
                            );
                        }
                        for (index, playlist) in state.playlists.iter().enumerate() {
                            ui.push_id(playlist.id, |ui| {
                                let rect = list_rows::allocate(ui, index, false);
                                let text_rect = egui::Rect::from_min_max(
                                    rect.min + egui::vec2(7.0, 7.0),
                                    rect.max - egui::vec2(42.0, 7.0),
                                );
                                if list_rows::name(
                                    ui,
                                    text_rect,
                                    &playlist.name,
                                    self.available(),
                                    egui::Sense::click(),
                                )
                                .clicked()
                                {
                                    self.submit(LibraryCommand::SelectPlaylist(Some(playlist.id)));
                                }
                                let mut buttons = ui.new_child(egui::UiBuilder::new().max_rect(
                                    egui::Rect::from_center_size(
                                        egui::pos2(rect.right() - 21.0, rect.center().y),
                                        egui::vec2(28.0, 28.0),
                                    ),
                                ));
                                let remove = icon_buttons::icon_buttons(
                                    &mut buttons,
                                    icon_buttons::Icon::Remove,
                                    self.available(),
                                    "Delete playlist",
                                );
                                #[cfg(test)]
                                self.playlist_remove.push(remove.rect);
                                if remove.clicked() {
                                    self.submit(LibraryCommand::DeletePlaylist(playlist.id));
                                }
                            });
                        }
                    });
            });
    }
}
