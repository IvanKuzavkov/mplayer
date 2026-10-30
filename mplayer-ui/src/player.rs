use super::*;

fn device_name(device: &str) -> &str {
    device.rsplit_once(" [").map_or(device, |(name, _)| name)
}

fn player_slider(
    ui: &mut egui::Ui,
    value: &mut f64,
    maximum: f64,
    width: f32,
    enabled: bool,
) -> egui::Response {
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = width;
        ui.spacing_mut().slider_rail_height = 2.0;
        ui.visuals_mut().selection.bg_fill = theme::accent(ui);
        ui.add_enabled(
            enabled,
            egui::Slider::new(value, 0.0..=maximum)
                .show_value(false)
                .trailing_fill(true),
        )
    })
    .inner
}

pub(super) fn record_icon(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, 8.0, egui::Color32::from_rgb(37, 36, 33));
    let center = rect.center();
    let radius = rect.width() * 0.37;
    let stroke_width = (rect.width() / 220.0).clamp(0.5, 1.25);
    painter.circle_filled(center, radius, egui::Color32::from_rgb(24, 28, 33));
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(stroke_width, egui::Color32::from_rgb(91, 101, 112)),
    );
    // Broad reflections keep the grooves legible even in the small library thumbnails.
    for (angle, alpha) in [(-2.35_f32, 48), (0.8, 30)] {
        let point = |distance: f32, offset: f32| {
            let angle = angle + offset;
            center + egui::vec2(angle.cos(), angle.sin()) * (radius * distance)
        };
        painter.add(egui::Shape::convex_polygon(
            vec![
                point(0.32, -0.24),
                point(0.98, -0.24),
                point(0.98, 0.24),
                point(0.32, 0.24),
            ],
            egui::Color32::from_rgba_unmultiplied(185, 199, 214, alpha),
            egui::Stroke::NONE,
        ));
    }
    for ratio in [0.4, 0.51, 0.62, 0.73, 0.84, 0.95] {
        painter.circle_stroke(
            center,
            radius * ratio,
            egui::Stroke::new(stroke_width, egui::Color32::from_rgb(65, 74, 84)),
        );
    }
    painter.circle_filled(center, radius * 0.3, theme::ACCENT);
    painter.circle_stroke(
        center,
        radius * 0.3,
        egui::Stroke::new(stroke_width, egui::Color32::from_rgb(255, 225, 126)),
    );
    painter.circle_filled(center, radius * 0.045, egui::Color32::from_rgb(13, 14, 16));
}

impl PlayerApp {
    pub(super) fn displayed_position(&self, playback: &mplayer_core::PlaybackSnapshot) -> f64 {
        self.seek_draft
            .or_else(|| self.pending_seek.map(|(_, position)| position as f64))
            .unwrap_or(playback.position_ms as f64)
            .clamp(0.0, playback.duration_ms.max(1) as f64)
    }

    pub(super) fn seek_to(&mut self, position_ms: u64) {
        let request_id = self.seek_request_id.wrapping_add(1).max(1);
        if self.submit(AppCommand::Seek {
            position_ms,
            request_id,
        }) {
            self.seek_request_id = request_id;
            self.pending_seek = Some((request_id, position_ms));
        }
        self.seek_draft = None;
    }

    pub(super) fn output_controls(&mut self, ui: &mut egui::Ui, state: &AppSnapshot) {
        let playback = &state.playback;
        ui.horizontal(|ui| {
            let mut volume = f64::from(playback.volume);
            let tooltip = format!("Volume: {}%", (volume * 100.0).round() as u32);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
            #[cfg(test)]
            {
                self.volume_icon = Some(rect);
            }
            response.on_hover_text(&tooltip);
            let center = rect.center();
            let color = ui.visuals().weak_text_color();
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    center + egui::vec2(-9.0, -3.0),
                    center + egui::vec2(-5.0, 3.0),
                ),
                1.0,
                color,
            );
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    center + egui::vec2(-5.0, -3.0),
                    center + egui::vec2(0.0, -7.0),
                    center + egui::vec2(0.0, 7.0),
                    center + egui::vec2(-5.0, 3.0),
                ],
                color,
                egui::Stroke::NONE,
            ));
            for radius in [4.0, 8.0] {
                let points = (-3..=3)
                    .map(|step| {
                        let angle = step as f32 * 0.25;
                        center + egui::vec2(1.0 + radius * angle.cos(), radius * angle.sin())
                    })
                    .collect();
                ui.painter()
                    .add(egui::Shape::line(points, egui::Stroke::new(1.3, color)));
            }
            let width = (ui.available_width() - 28.0 - ui.spacing().item_spacing.x).max(0.0);
            let volume_slider =
                player_slider(ui, &mut volume, 1.0, width, self.available()).on_hover_text(tooltip);
            #[cfg(test)]
            {
                self.volume_slider = Some(volume_slider.rect);
            }
            if volume_slider.changed() {
                self.submit(AppCommand::SetVolume(volume as f32));
            }
            // Keep device selection available when the current output has failed.
            let selected = playback
                .selected_device
                .as_deref()
                .map_or("System default", device_name);
            let audio_output_button = icon_buttons::icon_buttons(
                ui,
                icon_buttons::Icon::AudioOutput,
                self.available(),
                &format!("Output device: {selected}"),
            );
            #[cfg(test)]
            {
                self.output_button = Some(audio_output_button.rect);
            }
            egui::Popup::menu(&audio_output_button).show(|ui| {
                ui.label(
                    RichText::new("Output device")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
                if ui
                    .selectable_label(playback.selected_device.is_none(), "System default")
                    .clicked()
                {
                    self.submit(AppCommand::SetDevice(None));
                    ui.close();
                }
                for device in &playback.devices {
                    if ui
                        .selectable_label(
                            playback.selected_device.as_ref() == Some(device),
                            device_name(device),
                        )
                        .on_hover_text(device)
                        .clicked()
                    {
                        self.submit(AppCommand::SetDevice(Some(device.clone())));
                        ui.close();
                    }
                }
            });
        });
        ui.add_space(16.0);
    }

    pub(super) fn playback_message(&self, ui: &mut egui::Ui, state: &AppSnapshot) {
        let message = state
            .playback
            .error
            .as_ref()
            .map(|error| {
                (
                    format!("Playback error: {error}"),
                    ui.visuals().error_fg_color,
                )
            })
            .or_else(|| {
                state
                    .playback
                    .notification
                    .as_ref()
                    .map(|notice| (notice.clone(), ui.visuals().weak_text_color()))
            });
        let progress = ui.ctx().animate_bool_with_time(
            ui.scope_id().with("message_visible"),
            message.is_some(),
            0.5,
        );
        if let Some((message, color)) = message {
            let eased = 1.0 - (1.0 - progress).powi(3);
            let height = 2.0 * ui.text_style_height(&egui::TextStyle::Small);
            let rect = egui::Rect::from_min_size(
                ui.max_rect().min + egui::vec2(0.0, 8.0 * (1.0 - eased)),
                egui::vec2(ui.available_width(), height),
            );
            let mut message_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            message_ui.multiply_opacity(eased);
            egui::ScrollArea::vertical()
                .id_salt("playback_message")
                .min_scrolled_height(0.0)
                .max_height(height)
                .auto_shrink([false, false])
                .show(&mut message_ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&message).small().color(color))
                                .halign(egui::Align::Center)
                                .selectable(false)
                                .wrap(),
                        )
                        .on_hover_text(&message);
                    });
                });
        }
    }

    pub(super) fn player(&mut self, ui: &mut egui::Ui, state: &AppSnapshot, available_height: f32) {
        let playback = &state.playback;
        let title = playback
            .track
            .as_ref()
            .map_or_else(|| "Nothing playing".into(), track_title);
        let mut metadata = vec![(RichText::new(title).size(23.0).strong(), 2, 19.0)];
        if let Some(track) = &playback.track {
            metadata.push((
                RichText::new(theme::or(&track.metadata.artist, "Unknown artist"))
                    .size(14.0)
                    .color(ui.visuals().weak_text_color()),
                1,
                0.0,
            ));
            metadata.push((
                RichText::new(theme::or(&track.metadata.album, "Unknown album"))
                    .size(12.0)
                    .color(ui.visuals().weak_text_color()),
                1,
                0.0,
            ));
            let bits = track
                .metadata
                .bit_depth
                .map_or_else(String::new, |bits| format!(" · {bits} bit"));
            metadata.push((
                RichText::new(format!(
                    "{} · {:.1} kHz{}",
                    track.metadata.codec.to_uppercase(),
                    track.metadata.sample_rate as f32 / 1000.0,
                    bits
                ))
                .size(12.0)
                .color(theme::accent(ui)),
                1,
                10.0,
            ));
        } else {
            metadata.push((
                RichText::new("Choose a track from your library")
                    .small()
                    .color(ui.visuals().weak_text_color()),
                2,
                0.0,
            ));
        }
        let width = ui.available_width();
        let metadata: Vec<_> = metadata
            .into_iter()
            .map(|(text, rows, gap)| {
                let mut job = Arc::unwrap_or_clone(egui::WidgetText::from(text).into_layout_job(
                    ui.style(),
                    egui::FontSelection::Default,
                    egui::Align::Center,
                ));
                job.wrap.max_width = width;
                job.wrap.max_rows = rows;
                job.halign = egui::Align::Center;
                (ui.fonts_mut(|fonts| fonts.layout_job(job)), gap)
            })
            .collect();
        // Reserve the actual wrapped text height before sizing the artwork.
        let text_height: f32 = metadata
            .iter()
            .map(|(text, gap)| text.size().y + gap + ui.spacing().item_spacing.y)
            .sum();
        let size = width
            .min(280.0)
            .min((available_height - text_height - 10.0).max(0.0));
        ui.vertical_centered(|ui| {
            if let Some(texture) = self.cover.texture() {
                let aspect = texture.size_vec2().x / texture.size_vec2().y;
                let uv = if aspect > 1.0 {
                    let inset = (1.0 - 1.0 / aspect) * 0.5;
                    egui::Rect::from_min_max(egui::pos2(inset, 0.0), egui::pos2(1.0 - inset, 1.0))
                } else {
                    let inset = (1.0 - aspect) * 0.5;
                    egui::Rect::from_min_max(egui::pos2(0.0, inset), egui::pos2(1.0, 1.0 - inset))
                };
                ui.add(
                    egui::Image::from_texture((texture.id(), texture.size_vec2()))
                        .maintain_aspect_ratio(false)
                        .uv(uv)
                        .fit_to_exact_size(egui::vec2(size, size))
                        .corner_radius(13),
                );
            } else {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
                record_icon(ui.painter(), rect);
            }
            for (text, gap) in metadata {
                ui.add_space(gap);
                ui.add(
                    egui::Label::new(text)
                        .selectable(false)
                        .show_tooltip_when_elided(false),
                );
            }
            ui.add_space(10.0);
        });
    }

    pub(super) fn playback_controls(&mut self, ui: &mut egui::Ui, state: &AppSnapshot) {
        let playback = &state.playback;
        let mut position = self.displayed_position(playback);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(theme::duration(position as u64))
                    .size(12.0)
                    .monospace()
                    .color(ui.visuals().weak_text_color()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(theme::duration(playback.duration_ms))
                        .size(12.0)
                        .monospace()
                        .color(ui.visuals().weak_text_color()),
                );
            });
        });
        let duration = playback.duration_ms.max(1) as f64;
        {
            let response = player_slider(
                ui,
                &mut position,
                duration,
                ui.available_width(),
                self.available() && playback.track.is_some() && playback.duration_ms > 0,
            );
            #[cfg(test)]
            {
                self.seek_slider = Some(response.rect);
            }
            if response.dragged() {
                self.seek_draft = Some(position);
            }
            if response.drag_stopped() || (response.changed() && !response.dragged()) {
                self.seek_to(position as u64);
            }
        }
        ui.add_space(12.0);
        let controls = ui.horizontal_centered(|ui| {
            let total = 28.0 * 2.0 + 39.0 * 2.0 + 56.0 + ui.spacing().item_spacing.x * 4.0;
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            let available = self.available();
            let (icon, label, next_mode) = match state.play_mode {
                PlayMode::Sequential => (
                    icon_buttons::Icon::Sequential,
                    "Sequential — switch to repeat queue",
                    PlayMode::Repeat,
                ),
                PlayMode::Repeat => (
                    icon_buttons::Icon::Repeat,
                    "Repeat queue — switch to shuffle",
                    PlayMode::Shuffle,
                ),
                PlayMode::Shuffle => (
                    icon_buttons::Icon::Shuffle,
                    "Shuffle — switch to sequential",
                    PlayMode::Sequential,
                ),
            };
            let mode = icon_buttons::icon_buttons(ui, icon, available, label);
            #[cfg(test)]
            {
                self.mode_button = Some(mode.rect);
            }
            if mode.clicked() {
                self.submit(AppCommand::SetPlayMode(next_mode));
            }
            let previous = icon_buttons::icon_buttons(
                ui,
                icon_buttons::Icon::Previous,
                available && state.queue_index.is_some(),
                "Previous track",
            );
            #[cfg(test)]
            {
                self.transport_buttons[0] = Some(previous.rect);
            }
            if previous.clicked() {
                self.submit(AppCommand::Previous);
            }
            let playing = matches!(
                playback.state,
                PlaybackState::Playing | PlaybackState::Buffering
            );
            let play = icon_buttons::icon_buttons(
                ui,
                if playing {
                    icon_buttons::Icon::Pause
                } else {
                    icon_buttons::Icon::Play
                },
                available && (!state.queue.is_empty() || playback.track.is_some()),
                if playing { "Pause" } else { "Play" },
            );
            #[cfg(test)]
            {
                self.transport_buttons[1] = Some(play.rect);
            }
            if play.clicked() {
                self.submit(AppCommand::PauseResume);
            }
            let next = icon_buttons::icon_buttons(
                ui,
                icon_buttons::Icon::Next,
                available
                    && state.queue_index.is_some_and(|index| {
                        index + 1 < state.queue.len() || state.play_mode != PlayMode::Sequential
                    }),
                "Next track",
            );
            #[cfg(test)]
            {
                self.transport_buttons[2] = Some(next.rect);
            }
            if next.clicked() {
                self.submit(AppCommand::Next);
            }

            let queue =
                icon_buttons::icon_buttons(ui, icon_buttons::Icon::Queue, true, "Play queue");
            #[cfg(test)]
            {
                self.queue_button = Some(queue.rect);
            }
            if queue.clicked() {
                self.toggle_queue();
            }
        });
        ui.add_space(12.0);
        #[cfg(test)]
        {
            self.transport_rect = Some(controls.response.rect);
        }
        #[cfg(not(test))]
        let _ = controls;
    }
}
