use super::*;
use icon_buttons::{Icon, icon_buttons};

impl PlayerApp {
    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui) {
        for (view, label) in [
            (View::Library, "Media library"),
            (View::ScanFolders, "Scan folders"),
            (View::Playlists, "Playlists"),
        ] {
            if ui
                .add_sized(
                    [ui.available_width(), 36.0],
                    egui::Button::selectable(self.view == view, label),
                )
                .clicked()
            {
                self.view = view;
            }
        }
    }

    pub(super) fn scan_folders(&mut self, ui: &mut egui::Ui, state: &AppSnapshot) {
        egui::Panel::bottom("scan_folder_actions")
            .exact_size(45.0)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let add = icon_buttons(
                        ui,
                        Icon::Add,
                        self.available() && !self.folders.is_open(),
                        "Add music folder",
                    );
                    #[cfg(test)]
                    {
                        self.scan_actions[0] = Some(add.rect);
                    }
                    if add.clicked() {
                        self.folders.open(state.roots.last().cloned(), ui.ctx());
                    }
                    let rescan = icon_buttons(
                        ui,
                        Icon::Rescan,
                        self.available() && !state.scan.running && !state.roots.is_empty(),
                        "Rescan all music folders",
                    );
                    #[cfg(test)]
                    {
                        self.scan_actions[1] = Some(rescan.rect);
                    }
                    if rescan.clicked() {
                        self.submit(AppCommand::Rescan);
                    }
                    if state.scan.running {
                        ui.spinner();
                        ui.label(
                            RichText::new(format!("{} files scanned", state.scan.processed))
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                        if ui.small_button("Cancel").clicked() {
                            self.submit(AppCommand::CancelScan);
                        }
                    } else {
                        ui.label(
                            RichText::new(format!("{} folders", state.roots.len()))
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("scan_roots")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if state.roots.is_empty() {
                            ui.label(
                                RichText::new("Add a folder to find your music.")
                                    .color(ui.visuals().weak_text_color()),
                            );
                        }
                        for (index, path) in state.roots.iter().enumerate() {
                            ui.push_id(path, |ui| {
                                let rect = list_rows::allocate(ui, index, false);
                                let text_rect = egui::Rect::from_min_max(
                                    rect.min + egui::vec2(7.0, 7.0),
                                    rect.max - egui::vec2(42.0, 7.0),
                                );
                                list_rows::name(
                                    ui,
                                    text_rect,
                                    &path.display().to_string(),
                                    true,
                                    egui::Sense::hover(),
                                );
                                let mut buttons = ui.new_child(egui::UiBuilder::new().max_rect(
                                    egui::Rect::from_center_size(
                                        egui::pos2(rect.right() - 21.0, rect.center().y),
                                        egui::vec2(28.0, 28.0),
                                    ),
                                ));
                                let remove = icon_buttons(
                                    &mut buttons,
                                    Icon::Remove,
                                    self.available()
                                        && !state.scan.running
                                        && state.queued_imports == 0,
                                    "Remove folder from library and playlists; keep files on disk",
                                );
                                #[cfg(test)]
                                self.scan_remove.push(remove.rect);
                                if remove.clicked() {
                                    self.submit(AppCommand::RemoveScanFolder(path.clone()));
                                }
                            });
                        }
                        if state.queued_imports > 0 {
                            ui.label(
                                RichText::new(format!("{} paths queued", state.queued_imports))
                                    .small()
                                    .color(theme::accent(ui.visuals())),
                            );
                        }
                    });
            });
    }
}
