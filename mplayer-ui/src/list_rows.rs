use super::*;

pub(super) fn allocate(ui: &mut egui::Ui, index: usize, active: bool) -> egui::Rect {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 54.0), egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        4.0,
        if active {
            ui.visuals().selection.bg_fill
        } else if index.is_multiple_of(2) {
            ui.visuals().faint_bg_color
        } else {
            ui.visuals().panel_fill
        },
    );
    rect
}

pub(super) fn name(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    name: &str,
    enabled: bool,
    sense: egui::Sense,
) -> egui::Response {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child
        .add_enabled_ui(enabled, |ui| {
            ui.put(
                rect,
                egui::Label::new(RichText::new(name).size(15.0).strong())
                    .halign(egui::Align::Min)
                    .truncate()
                    .selectable(false)
                    .sense(sense),
            )
            .on_hover_text(name)
        })
        .inner
}
