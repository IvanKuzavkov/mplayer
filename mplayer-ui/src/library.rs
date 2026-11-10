mod albums;
mod folders;

use super::*;
use mplayer_core::PAGE_SIZE;

#[derive(Clone, Copy)]
pub(super) enum TrackAction {
    Add,
    RemoveQueue(usize),
    RemovePlaylist(i64),
}

impl PlayerApp {
    fn library_page(&mut self, offset: u64) {
        let query = self.search.clone();
        let command = match (&self.tab, &self.group) {
            (LibraryTab::Folders, None) => AppCommand::ListFolders { query, offset },
            (LibraryTab::Albums, None) => AppCommand::ListAlbums { query, offset },
            _ => AppCommand::BrowseLibrary {
                filter: self.group.clone().unwrap_or_default(),
                query,
                offset,
            },
        };
        self.submit(command);
    }
    pub(super) fn open_group(&mut self, filter: LibraryFilter) {
        if self.submit(AppCommand::BrowseLibrary {
            filter: filter.clone(),
            query: String::new(),
            offset: 0,
        }) {
            self.group = Some(filter);
            self.search.clear();
        }
    }
    fn group_back(&mut self, ui: &mut egui::Ui, state: &AppSnapshot) {
        let back = icon_buttons::icon_buttons(ui, icon_buttons::Icon::Back, true, "Back");
        #[cfg(test)]
        {
            self.back_button = Some(back.rect);
        }
        if back.clicked() {
            self.group = None;
            self.search = match self.tab {
                LibraryTab::Folders => state.folders.query.clone(),
                LibraryTab::Albums => state.albums.query.clone(),
                LibraryTab::Tracks => String::new(),
            };
        }
    }

    fn album_header(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppSnapshot,
        key: &mplayer_core::AlbumKey,
    ) {
        let summary = state.albums.albums.iter().find(|album| album.key == *key);
        // The old album's page may still be visible while a new request is in flight.
        let tracks = if state.library_filter == LibraryFilter::Album(key.clone()) {
            state.library.tracks.as_slice()
        } else {
            &[]
        };
        let artwork_path = summary
            .and_then(|album| album.artwork_path.as_deref())
            .or_else(|| {
                tracks
                    .iter()
                    .find(|track| !track.missing)
                    .map(|track| track.path.as_path())
            });
        let year = summary
            .and_then(|album| album.year)
            .or_else(|| tracks.iter().find_map(|track| track.metadata.year));
        let cover_size = (ui.available_width() * 0.32).clamp(80.0, 140.0);
        ui.horizontal_top(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(cover_size, cover_size), egui::Sense::hover());
            #[cfg(test)]
            {
                self.album_cover = Some(rect);
            }
            let texture = artwork_path.and_then(|path| self.thumbnails.texture(path));
            artwork::paint_cover(ui, texture, rect, 6);
            self.album_artwork_actions(
                ui,
                rect,
                key,
                summary.map_or(!tracks.is_empty(), |album| album.track_count > 0),
            );
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                let title = theme::or(&key.title, "Unknown album");
                ui.add(
                    egui::Label::new(
                        RichText::new(title)
                            .size(22.0)
                            .strong()
                            .color(ui.visuals().strong_text_color()),
                    )
                    .truncate(),
                )
                .on_hover_text(title);
                let artist = theme::or(&key.artist, "Unknown artist");
                ui.add(
                    egui::Label::new(
                        RichText::new(artist)
                            .size(15.0)
                            .color(ui.visuals().weak_text_color()),
                    )
                    .truncate(),
                )
                .on_hover_text(artist);
                if let Some(year) = year {
                    ui.label(RichText::new(year.to_string()).color(ui.visuals().weak_text_color()));
                }
            });
        });
    }

    pub(super) fn library(&mut self, ui: &mut egui::Ui, state: &AppSnapshot) {
        let mut skip_pagination = false;
        ui.horizontal(|ui| {
            for (tab, label) in [
                (LibraryTab::Tracks, "Tracks"),
                (LibraryTab::Folders, "Folders"),
                (LibraryTab::Albums, "Albums"),
            ] {
                if ui.selectable_label(self.tab == tab, label).clicked() && self.tab != tab {
                    let query = match tab {
                        LibraryTab::Tracks => String::new(),
                        LibraryTab::Folders => state.folders.query.clone(),
                        LibraryTab::Albums => state.albums.query.clone(),
                    };
                    let command = match tab {
                        LibraryTab::Tracks => AppCommand::BrowseLibrary {
                            filter: LibraryFilter::All,
                            query: query.clone(),
                            offset: 0,
                        },
                        LibraryTab::Folders => AppCommand::ListFolders {
                            query: query.clone(),
                            offset: 0,
                        },
                        LibraryTab::Albums => AppCommand::ListAlbums {
                            query: query.clone(),
                            offset: 0,
                        },
                    };
                    if self.submit(command) {
                        self.tab = tab;
                        self.group = None;
                        self.search = query;
                    }
                }
            }
        });
        ui.add_space(8.0);
        if let Some(filter) = self.group.clone() {
            self.group_back(ui, state);
            ui.add_space(8.0);
            match filter {
                LibraryFilter::Album(key) => self.album_header(ui, state, &key),
                LibraryFilter::Folder(path) => self.folder_header(ui, state, &path),
                LibraryFilter::All => {}
            }
        } else {
            ui.horizontal(|ui| {
                let hint = match (self.tab, self.group.is_some()) {
                    (LibraryTab::Folders, false) => "Search folders",
                    (LibraryTab::Albums, false) => "Search albums or artists",
                    _ => "Search tracks, artists, or albums",
                };
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text(hint)
                            .desired_width((ui.available_width() - 38.0).max(100.0)),
                    )
                    .changed()
                {
                    self.library_page(0);
                }
                let clear = icon_buttons::icon_buttons(
                    ui,
                    icon_buttons::Icon::Clear,
                    !self.search.is_empty() && self.available(),
                    "Clear search",
                );
                #[cfg(test)]
                {
                    self.clear_button = Some(clear.rect);
                }
                if clear.clicked() {
                    self.search.clear();
                    self.library_page(0);
                }
            });
        }
        ui.add_space(10.0);
        ui.separator();
        let group_list = self.group.is_none() && self.tab != LibraryTab::Tracks;
        let (total, offset, count) = if group_list {
            match self.tab {
                LibraryTab::Folders => (
                    state.folders.total,
                    state.folders.offset,
                    state.folders.folders.len(),
                ),
                _ => (
                    state.albums.total,
                    state.albums.offset,
                    state.albums.albums.len(),
                ),
            }
        } else {
            (
                state.library.total,
                state.library.offset,
                state.library.tracks.len(),
            )
        };
        let height = (ui.available_height() - 44.0).max(40.0);
        let expected_filter = self.group.clone().unwrap_or_default();
        if !group_list && state.library_filter != expected_filter {
            ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                ui.spinner();
                ui.label("Loading tracks…");
            });
        } else if count == 0 {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.add_space((height * 0.2).max(12.0));
                    ui.add(
                        egui::Label::new(
                            RichText::new(if self.search.is_empty() {
                                "Your music starts here"
                            } else {
                                "No matching music"
                            })
                            .size(22.0)
                            .strong(),
                        )
                        .selectable(false)
                        .show_tooltip_when_elided(false),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Choose a music folder or drop audio files into the window.",
                            )
                            .color(ui.visuals().weak_text_color()),
                        )
                        .selectable(false)
                        .show_tooltip_when_elided(false),
                    );
                    skip_pagination = true;
                    ui.add_space(12.0);
                    if ui.button("Add music folders…").clicked() {
                        self.view = View::ScanFolders;
                    }
                },
            );
        } else if group_list && self.tab == LibraryTab::Albums {
            let columns = ((ui.available_width() + 16.0) / 176.0).floor().max(1.0) as usize;
            let rows = count.div_ceil(columns);
            let width = (ui.available_width() - 16.0 * (columns - 1) as f32) / columns as f32;
            let cover_size = width - 20.0;
            let row_height = cover_size + 70.0;
            let output = egui::ScrollArea::vertical()
                .id_salt("album_grid")
                .max_height(height)
                .auto_shrink([false, false])
                .show_rows(ui, row_height, rows, |ui, range| {
                    for row in range {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 16.0;
                            for index in row * columns..((row + 1) * columns).min(count) {
                                let album = &state.albums.albums[index];
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(width, row_height),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    rect,
                                    8.0,
                                    if response.hovered() {
                                        ui.visuals().widgets.hovered.bg_fill
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    },
                                );
                                let icon = egui::Rect::from_min_size(
                                    rect.min + egui::vec2(10.0, 10.0),
                                    egui::vec2(cover_size, cover_size),
                                );
                                let texture = album
                                    .artwork_path
                                    .as_deref()
                                    .and_then(|path| self.thumbnails.texture(path));
                                artwork::paint_cover(ui, texture, icon, 6);
                                let text = egui::Rect::from_min_max(
                                    egui::pos2(icon.left(), icon.bottom() + 12.0),
                                    rect.max - egui::vec2(10.0, 7.0),
                                );
                                let mut text_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .id_salt(("album_text", index))
                                        .max_rect(text)
                                        .layout(egui::Layout::top_down(egui::Align::Min)),
                                );
                                text_ui.set_clip_rect(text.intersect(ui.clip_rect()));
                                text_ui.add(
                                    egui::Label::new(
                                        RichText::new(theme::or(&album.key.title, "Unknown album"))
                                            .strong(),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                                text_ui.add(
                                    egui::Label::new(
                                        RichText::new(theme::or(
                                            &album.key.artist,
                                            "Unknown artist",
                                        ))
                                        .small()
                                        .color(ui.visuals().weak_text_color()),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                                let response = ui.interact(
                                    rect,
                                    ui.scope_id().with((
                                        "album_card",
                                        &album.key.title,
                                        &album.key.artist,
                                    )),
                                    egui::Sense::click(),
                                );
                                let over_actions = self.album_artwork_actions(
                                    ui,
                                    icon,
                                    &album.key,
                                    album.track_count > 0,
                                );
                                if response
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                                    && !over_actions
                                {
                                    self.open_group(LibraryFilter::Album(album.key.clone()));
                                }
                            }
                        });
                    }
                });
            #[cfg(test)]
            {
                self.library_scroll = Some(output.inner_rect);
            }
            #[cfg(not(test))]
            let _ = output;
        } else if group_list {
            let output = egui::ScrollArea::vertical()
                .id_salt("library_folders")
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (index, folder) in state.folders.folders.iter().enumerate() {
                        self.folder_row(ui, state, folder, index);
                    }
                });
            #[cfg(test)]
            {
                self.library_scroll = Some(output.inner_rect);
            }
            #[cfg(not(test))]
            let _ = output;
        } else {
            let output = egui::ScrollArea::vertical()
                .id_salt(("library_rows", self.tab))
                .max_height(height)
                .auto_shrink([false, false])
                .show_rows(ui, 54.0, count, |ui, range| {
                    for index in range {
                        let track = &state.library.tracks[index];
                        ui.push_id(track.id, |ui| {
                            self.track_row(
                                ui,
                                track,
                                state,
                                index,
                                TrackAction::Add,
                                self.group.is_none(),
                            );
                        });
                    }
                });
            #[cfg(test)]
            {
                self.library_scroll = Some(output.inner_rect);
            }
            #[cfg(not(test))]
            let _ = output;
        }
        ui.add_space(6.0);
        if !skip_pagination {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(offset > 0, egui::Button::new("‹ Previous"))
                    .clicked()
                {
                    self.library_page(offset.saturating_sub(PAGE_SIZE));
                }
                let end = (offset + count as u64).min(total);
                ui.label(
                    RichText::new(format!(
                        "{}–{} / {}",
                        if total == 0 { 0 } else { offset + 1 },
                        end,
                        total
                    ))
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
                if ui
                    .add_enabled(end < total, egui::Button::new("Next ›"))
                    .clicked()
                {
                    self.library_page(offset + PAGE_SIZE);
                }
            });
        }
    }
    pub(super) fn track_row(
        &mut self,
        ui: &mut egui::Ui,
        track: &Track,
        state: &AppSnapshot,
        index: usize,
        action: TrackAction,
        show_artwork: bool,
    ) -> egui::Response {
        let queue_index = match action {
            TrackAction::RemoveQueue(index) => Some(index),
            _ => None,
        };
        let active = queue_index.map_or_else(
            || {
                state
                    .playback
                    .track
                    .as_ref()
                    .is_some_and(|current| current.id == track.id)
            },
            |index| state.queue_index == Some(index),
        );
        let enabled = !track.missing && self.available();
        let rect = list_rows::allocate(ui, index, active);
        let content = rect.shrink(7.0);
        let button_rect = egui::Rect::from_center_size(
            egui::pos2(content.right() - 14.0, content.center().y),
            egui::vec2(28.0, 28.0),
        );
        let duration_rect = egui::Rect::from_min_max(
            egui::pos2(button_rect.left() - 66.0, content.top()),
            egui::pos2(button_rect.left() - 10.0, content.bottom()),
        );
        let leading_rect = egui::Rect::from_min_max(
            content.min,
            egui::pos2(
                content.left() + if show_artwork { 40.0 } else { 28.0 },
                content.bottom(),
            ),
        );
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(leading_rect.right() + 8.0, content.top()),
            egui::pos2(duration_rect.left() - 10.0, content.bottom()),
        );
        if show_artwork {
            let texture = if track.missing {
                None
            } else {
                self.thumbnails.texture(&track.path)
            };
            artwork::paint_cover(ui, texture, leading_rect, 4);
        } else {
            ui.put(
                leading_rect,
                egui::Label::new(
                    RichText::new(format!(
                        "{:02}",
                        track
                            .metadata
                            .track_number
                            .map_or(index + 1, |n| n as usize)
                    ))
                    .size(12.0)
                    .color(if active {
                        theme::accent(ui.visuals())
                    } else {
                        ui.visuals().weak_text_color()
                    }),
                )
                .halign(egui::Align::Min)
                .selectable(false)
                .sense(egui::Sense::hover()),
            );
        }
        let mut text_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("track_text")
                .max_rect(text_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        text_ui.set_clip_rect(text_rect.intersect(ui.clip_rect()));
        text_ui.spacing_mut().item_spacing.y = 3.0;
        text_ui.add(
            egui::Label::new(RichText::new(track_title(track)).size(15.0).strong().color(
                if track.missing {
                    ui.visuals().error_fg_color
                } else if active {
                    theme::accent(ui.visuals())
                } else {
                    ui.visuals().strong_text_color()
                },
            ))
            .truncate()
            .selectable(false)
            .sense(egui::Sense::hover()),
        );
        text_ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "{} · {}{}",
                    theme::or(&track.metadata.artist, "Unknown artist"),
                    theme::or(&track.metadata.album, "Unknown album"),
                    if track.missing { " · Missing" } else { "" },
                ))
                .size(12.0)
                .color(ui.visuals().weak_text_color()),
            )
            .truncate()
            .selectable(false)
            .sense(egui::Sense::hover()),
        );
        ui.painter().text(
            duration_rect.right_center(),
            egui::Align2::RIGHT_CENTER,
            theme::duration(track.metadata.duration_ms),
            egui::FontId::monospace(12.0),
            ui.visuals().weak_text_color(),
        );
        let mut button_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("enqueue")
                .max_rect(button_rect),
        );
        let button = icon_buttons::icon_buttons(
            &mut button_ui,
            if matches!(action, TrackAction::Add) {
                icon_buttons::Icon::Add
            } else {
                icon_buttons::Icon::Remove
            },
            self.available() && (!matches!(action, TrackAction::Add) || !track.missing),
            match action {
                TrackAction::Add => "Add to queue",
                TrackAction::RemoveQueue(_) => "Remove from queue",
                TrackAction::RemovePlaylist(_) => "Remove from playlist",
            },
        );
        #[cfg(test)]
        self.queue_buttons.push(button.rect);
        if button.clicked() {
            self.submit(match action {
                TrackAction::Add => AppCommand::Enqueue(track.clone()),
                TrackAction::RemoveQueue(index) => AppCommand::RemoveQueue(index),
                TrackAction::RemovePlaylist(id) => AppCommand::RemovePlaylistEntry(id),
            });
        }
        let response = ui
            .interact(
                egui::Rect::from_min_max(
                    rect.min,
                    egui::pos2(button_rect.left() - 5.0, rect.bottom()),
                ),
                ui.scope_id().with("track_row"),
                if matches!(action, TrackAction::RemovePlaylist(_)) && self.available() {
                    egui::Sense::click_and_drag()
                } else {
                    egui::Sense::click()
                },
            )
            .on_hover_text(format!(
                "{}Double-click to play\n{}",
                if matches!(action, TrackAction::RemovePlaylist(_)) {
                    "Drag to reorder · "
                } else {
                    ""
                },
                track.path.display()
            ))
            .on_hover_cursor(if matches!(action, TrackAction::RemovePlaylist(_)) {
                egui::CursorIcon::Grab
            } else {
                egui::CursorIcon::Default
            });
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, enabled, track_title(track))
        });
        if enabled && response.double_clicked() {
            self.submit(
                queue_index.map_or_else(|| AppCommand::Play(track.clone()), AppCommand::PlayQueue),
            );
        }
        response.context_menu(|ui| {
            if ui
                .add_enabled(enabled, egui::Button::new("Add to queue"))
                .clicked()
            {
                self.submit(AppCommand::Enqueue(track.clone()));
                ui.close();
            }
            if let Some(index) = queue_index
                && ui.button("Remove from queue").clicked()
            {
                self.submit(AppCommand::RemoveQueue(index));
                ui.close();
            }
            if let TrackAction::RemovePlaylist(id) = action
                && ui.button("Remove from playlist").clicked()
            {
                self.submit(AppCommand::RemovePlaylistEntry(id));
                ui.close();
            }
            if !state.playlists.is_empty() {
                ui.separator();
                ui.label("Add to playlist");
            }
            for playlist in &state.playlists {
                if ui.button(&playlist.name).clicked() {
                    self.submit(AppCommand::AddToPlaylist {
                        playlist_id: playlist.id,
                        track_id: track.id,
                    });
                    ui.close();
                }
            }
        });
        response
    }
    pub(super) fn queue(&mut self, ui: &mut egui::Ui, state: &AppSnapshot) {
        ui.horizontal(|ui| {
            let back = icon_buttons::icon_buttons(ui, icon_buttons::Icon::Back, true, "Back");
            #[cfg(test)]
            {
                self.back_button = Some(back.rect);
            }
            if back.clicked() {
                self.toggle_queue();
            }
            ui.heading("Play queue");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let clear = icon_buttons::icon_buttons(
                    ui,
                    icon_buttons::Icon::ClearAll,
                    self.available() && !state.queue.is_empty(),
                    "Clear queue",
                );
                #[cfg(test)]
                {
                    self.clear_button = Some(clear.rect);
                }
                if clear.clicked() {
                    self.submit(AppCommand::ClearQueue);
                }
            });
        });
        ui.label(
            RichText::new(format!("{} tracks", state.queue.len()))
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .id_salt("queue")
            .auto_shrink([false, false])
            .show_rows(ui, 54.0, state.queue.len(), |ui, range| {
                for index in range {
                    ui.push_id(index, |ui| {
                        self.track_row(
                            ui,
                            &state.queue[index],
                            state,
                            index,
                            TrackAction::RemoveQueue(index),
                            true,
                        );
                    });
                }
            });
    }
}
