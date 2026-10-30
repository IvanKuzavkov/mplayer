use super::*;

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Add,
    Remove,
    Rescan,
    Expand,
    Collapse,
    Queue,
    AudioOutput,
    Back,
    Clear,
    ClearAll,
    Previous,
    Play,
    PlaySmall,
    Pause,
    Next,
    Sequential,
    Repeat,
    Shuffle,
}

pub(super) fn icon_buttons(
    ui: &mut egui::Ui,
    icon: Icon,
    enabled: bool,
    label: &str,
) -> egui::Response {
    let prominent = matches!(icon, Icon::Play | Icon::Pause);
    let compact_play = matches!(icon, Icon::PlaySmall);
    let transport = prominent || matches!(icon, Icon::Previous | Icon::Next);
    let size = if prominent {
        56.0
    } else if transport {
        39.0
    } else {
        28.0
    };
    let button = egui::Button::new("")
        .frame(transport)
        .min_size(egui::vec2(size, size));
    let button = if transport {
        button
            .fill(if prominent {
                theme::accent(ui)
            } else {
                ui.visuals().extreme_bg_color
            })
            .corner_radius(size as u8 / 2)
    } else if compact_play {
        button.corner_radius(14)
    } else {
        button
    };
    let response = ui
        .add_enabled(enabled, button)
        .on_hover_text(label)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let color = if !enabled {
        ui.visuals()
            .widgets
            .noninteractive
            .fg_stroke
            .color
            .gamma_multiply(ui.visuals().disabled_alpha())
    } else if prominent {
        ui.visuals().extreme_bg_color
    } else if response.hovered() || response.has_focus() {
        ui.visuals().widgets.hovered.fg_stroke.color
    } else {
        ui.visuals().weak_text_color()
    };
    let center = response.rect.center();
    let stroke = egui::Stroke::new(1.5, color);
    if compact_play {
        ui.painter().circle_stroke(center, size * 0.5 - 1.0, stroke);
    }
    match icon {
        Icon::ClearAll => {
            for (y, offset) in [(-5.0, 3.0), (0.0, 0.0), (5.0, -3.0)] {
                ui.painter().line_segment(
                    [
                        center + egui::vec2(-7.0 + offset, y),
                        center + egui::vec2(7.0 + offset, y),
                    ],
                    stroke,
                );
            }
        }
        Icon::Expand | Icon::Collapse => {
            let direction = if matches!(icon, Icon::Expand) {
                1.0
            } else {
                -1.0
            };
            ui.painter().add(egui::Shape::line(
                vec![
                    center + egui::vec2(-5.0, -2.5 * direction),
                    center + egui::vec2(0.0, 2.5 * direction),
                    center + egui::vec2(5.0, -2.5 * direction),
                ],
                stroke,
            ));
        }
        Icon::Back => {
            ui.painter().add(egui::Shape::line(
                vec![
                    center + egui::vec2(-1.0, -7.0),
                    center + egui::vec2(-8.0, 0.0),
                    center + egui::vec2(-1.0, 7.0),
                ],
                stroke,
            ));
            ui.painter().line_segment(
                [
                    center + egui::vec2(-8.0, 0.0),
                    center + egui::vec2(8.0, 0.0),
                ],
                stroke,
            );
        }
        Icon::Clear => {
            for direction in [-1.0, 1.0] {
                ui.painter().line_segment(
                    [
                        center + egui::vec2(-5.0, direction * -5.0),
                        center + egui::vec2(5.0, direction * 5.0),
                    ],
                    stroke,
                );
            }
        }
        Icon::Play | Icon::PlaySmall | Icon::Previous | Icon::Next => {
            let direction = if matches!(icon, Icon::Previous) {
                -1.0
            } else {
                1.0
            };
            let x = if prominent || compact_play {
                1.0
            } else {
                -direction * 2.0
            };
            let scale = if compact_play { 0.5 } else { 1.0 };
            let points = vec![
                center + egui::vec2(x - direction * 5.4, -9.0) * scale,
                center + egui::vec2(x + direction * 7.2, 0.0) * scale,
                center + egui::vec2(x - direction * 5.4, 9.0) * scale,
            ];
            if prominent || compact_play {
                ui.painter().add(egui::Shape::convex_polygon(
                    points,
                    color,
                    egui::Stroke::NONE,
                ));
            } else {
                ui.painter().add(egui::Shape::closed_line(points, stroke));
                let x = direction * 10.0;
                ui.painter().line_segment(
                    [center + egui::vec2(x, -9.0), center + egui::vec2(x, 9.0)],
                    stroke,
                );
            }
        }
        Icon::Pause => {
            for x in [-4.0, 4.0] {
                ui.painter().line_segment(
                    [center + egui::vec2(x, -10.0), center + egui::vec2(x, 10.0)],
                    egui::Stroke::new(3.0, color),
                );
            }
        }
        Icon::Sequential => {
            for y in [-4.0, 4.0] {
                let end = center + egui::vec2(8.0, y);
                ui.painter()
                    .line_segment([center + egui::vec2(-8.0, y), end], stroke);
                arrowhead(ui.painter(), end, egui::vec2(1.0, 0.0), stroke);
            }
        }
        Icon::Repeat => {
            for offset in [0.0, std::f32::consts::PI] {
                let points: Vec<_> = (0..=20)
                    .map(|index| {
                        let angle = offset + 0.2 + index as f32 / 20.0 * 2.5;
                        center + egui::vec2(angle.cos(), angle.sin()) * 8.0
                    })
                    .collect();
                let end = points[20];
                let direction = (end - points[19]).normalized();
                ui.painter().add(egui::Shape::line(points, stroke));
                arrowhead(ui.painter(), end, direction, stroke);
            }
        }
        Icon::Shuffle => {
            let points: Vec<_> = (0..=64)
                .map(|index| {
                    let angle = index as f32 / 64.0 * std::f32::consts::TAU;
                    center + egui::vec2(angle.sin() * 10.0, (2.0 * angle).sin() * 5.0)
                })
                .collect();
            let end = points[29];
            let direction = (points[30] - points[28]).normalized();
            ui.painter().add(egui::Shape::line(points, stroke));
            arrowhead(ui.painter(), end, direction, stroke);
        }
        Icon::Add | Icon::Remove => {
            let sizes = [egui::vec2(14.0, 1.5), egui::vec2(1.5, 14.0)];
            for size in sizes
                .iter()
                .take(if matches!(icon, Icon::Add) { 2 } else { 1 })
            {
                let arm = egui::Rect::from_center_size(center, *size);
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![
                        arm.left_top(),
                        arm.right_top(),
                        arm.right_bottom(),
                        arm.left_bottom(),
                    ],
                    color,
                    egui::Stroke::NONE,
                ));
            }
        }
        Icon::Rescan => {
            let points = (0..=24)
                .map(|index| {
                    let angle = std::f32::consts::TAU * (0.12 + index as f32 / 24.0 * 0.78);
                    center + egui::vec2(angle.cos(), angle.sin()) * 7.0
                })
                .collect();
            ui.painter().add(egui::Shape::line(points, stroke));
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    center + egui::vec2(7.5, -7.0),
                    center + egui::vec2(7.5, 0.0),
                    center + egui::vec2(1.0, -3.5),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        Icon::Queue => {
            for y in [-5.0, 0.0, 5.0] {
                ui.painter().line_segment(
                    [center + egui::vec2(-7.0, y), center + egui::vec2(7.0, y)],
                    stroke,
                );
            }
        }
        Icon::AudioOutput => {
            ui.painter().rect_stroke(
                egui::Rect::from_center_size(center - egui::vec2(0.0, 2.0), egui::vec2(16.0, 11.0)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            ui.painter().line_segment(
                [center + egui::vec2(0.0, 4.0), center + egui::vec2(0.0, 7.0)],
                stroke,
            );
            ui.painter().line_segment(
                [
                    center + egui::vec2(-5.0, 7.0),
                    center + egui::vec2(5.0, 7.0),
                ],
                stroke,
            );
        }
    }
    response
}

pub(super) fn folder_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let point = |x, y| rect.min + egui::vec2(x * rect.width(), y * rect.height());
    painter.add(egui::Shape::closed_line(
        vec![
            point(0.1, 0.25),
            point(0.4, 0.25),
            point(0.5, 0.37),
            point(0.9, 0.37),
            point(0.9, 0.8),
            point(0.1, 0.8),
        ],
        egui::Stroke::new(2.0, color),
    ));
}

pub(super) fn playlist_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    for fraction in [0.3, 0.5, 0.7] {
        let y = rect.top() + rect.height() * fraction;
        painter.circle_filled(egui::pos2(rect.left() + rect.width() * 0.2, y), 2.0, color);
        painter.line_segment(
            [
                egui::pos2(rect.left() + rect.width() * 0.35, y),
                egui::pos2(rect.left() + rect.width() * 0.85, y),
            ],
            egui::Stroke::new(2.0, color),
        );
    }
}

fn arrowhead(
    painter: &egui::Painter,
    tip: egui::Pos2,
    direction: egui::Vec2,
    stroke: egui::Stroke,
) {
    let side = egui::vec2(-direction.y, direction.x);
    painter.add(egui::Shape::line(
        vec![
            tip - direction * 3.5 + side * 3.0,
            tip,
            tip - direction * 3.5 - side * 3.0,
        ],
        stroke,
    ));
}
