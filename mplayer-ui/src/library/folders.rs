use crate::*;
use mplayer_core::{FolderSummary, ScanStatus};

fn folder_status(ui: &mut egui::Ui, folder: &FolderSummary) {
    let status = if folder.status == ScanStatus::Ready {
        "OK".into()
    } else {
        folder.status.to_string()
    };
    ui.label(
        RichText::new(format!("{} tracks", folder.track_count))
            .size(15.0)
            .color(ui.visuals().weak_text_color()),
    );
    ui.add(
        egui::Label::new(
            RichText::new(
                folder
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("Status: {status}")),
            )
            .color(if folder.error.is_some() {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().weak_text_color()
            }),
        )
        .wrap(),
    );
}

impl PlayerApp {
    pub(super) fn folder_header(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppSnapshot,
        path: &std::path::Path,
    ) {
        let icon_size = (ui.available_width() * 0.32).clamp(80.0, 140.0);
        ui.horizontal_top(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(icon_size, icon_size), egui::Sense::hover());
            icon_buttons::folder_icon(ui.painter(), rect, theme::accent(ui));
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(path.display().to_string())
                            .size(22.0)
                            .strong()
                            .color(ui.visuals().strong_text_color()),
                    )
                    .wrap(),
                );
                if let Some(folder) = state
                    .folders
                    .folders
                    .iter()
                    .find(|folder| folder.path == path)
                {
                    folder_status(ui, folder);
                } else {
                    ui.label("Folder information unavailable");
                }
            });
        });
    }

    pub(super) fn folder_row(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppSnapshot,
        folder: &FolderSummary,
        index: usize,
    ) {
        ui.push_id(&folder.path, |ui| {
            let mut details = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                ui.make_persistent_id("details"),
                false,
            );
            let rect = list_rows::allocate(ui, index, false);
            let text_rect = egui::Rect::from_min_max(
                rect.min + egui::vec2(8.0, 5.0),
                rect.max - egui::vec2(80.0, 5.0),
            );
            let mut text_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(text_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            let name = folder
                .path
                .file_name()
                .unwrap_or(folder.path.as_os_str())
                .to_string_lossy();
            text_ui.add(
                egui::Label::new(RichText::new(name.as_ref()).strong())
                    .truncate()
                    .selectable(false),
            );
            text_ui.label(RichText::new(folder.status.to_string()).small().color(
                if folder.status == ScanStatus::Error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().weak_text_color()
                },
            ));
            if ui
                .interact(
                    text_rect,
                    ui.make_persistent_id("open"),
                    egui::Sense::click(),
                )
                .on_hover_text(name.as_ref())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                self.open_group(LibraryFilter::Folder(folder.path.clone()));
            }
            let mut buttons = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_size(
                        egui::pos2(rect.right() - 68.0, rect.center().y - 14.0),
                        egui::vec2(64.0, 28.0),
                    ))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            let open = details.is_open();
            let expand = icon_buttons::icon_buttons(
                &mut buttons,
                if open {
                    icon_buttons::Icon::Collapse
                } else {
                    icon_buttons::Icon::Expand
                },
                true,
                if open {
                    "Hide folder details"
                } else {
                    "Show folder details"
                },
            );
            if expand.clicked() {
                details.toggle(ui);
            }
            let rescan = icon_buttons::icon_buttons(
                &mut buttons,
                icon_buttons::Icon::Rescan,
                self.available() && !state.scan.running,
                "Rescan folder",
            );
            #[cfg(test)]
            self.folder_actions.push([expand.rect, rescan.rect]);
            if rescan.clicked() {
                self.submit(AppCommand::RescanFolder(folder.id));
            }
            details.show_body_unindented(ui, |ui| {
                egui::Frame::new().inner_margin(8).show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(folder.path.display().to_string())
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        )
                        .wrap(),
                    );
                    folder_status(ui, folder);
                });
            });
        });
    }
}
