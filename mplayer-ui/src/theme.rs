use eframe::egui::{self, Color32};

pub const ACCENT: Color32 = Color32::from_rgb(221, 190, 83);

pub fn accent(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        ACCENT
    } else {
        Color32::from_rgb(125, 91, 0)
    }
}

pub fn apply(context: &egui::Context) {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        let dark = theme == egui::Theme::Dark;
        let mut visuals = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = if dark {
            Color32::from_rgb(30, 31, 35)
        } else {
            Color32::from_rgb(253, 252, 249)
        };
        visuals.window_fill = visuals.panel_fill;
        visuals.extreme_bg_color = if dark {
            Color32::from_rgb(19, 20, 23)
        } else {
            Color32::from_rgb(246, 245, 241)
        };
        visuals.weak_text_color = Some(if dark {
            Color32::from_rgb(164, 166, 171)
        } else {
            Color32::from_rgb(92, 94, 99)
        });
        visuals.selection.bg_fill = if dark {
            Color32::from_rgb(80, 69, 34)
        } else {
            Color32::from_rgb(244, 225, 166)
        };
        visuals.selection.stroke = egui::Stroke::new(
            1.0,
            if dark {
                ACCENT
            } else {
                Color32::from_rgb(125, 91, 0)
            },
        );
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(
            1.0,
            if dark {
                Color32::from_rgb(53, 54, 60)
            } else {
                Color32::from_rgb(200, 199, 193)
            },
        );
        visuals.widgets.active.bg_fill = if dark {
            Color32::from_rgb(65, 57, 34)
        } else {
            Color32::from_rgb(235, 216, 157)
        };
        visuals.widgets.hovered.bg_fill = if dark {
            Color32::from_rgb(58, 59, 64)
        } else {
            Color32::from_rgb(234, 233, 228)
        };
        visuals.faint_bg_color = if dark {
            Color32::from_rgb(37, 38, 42)
        } else {
            Color32::from_rgb(248, 247, 243)
        };
        visuals.error_fg_color = if dark {
            Color32::LIGHT_RED
        } else {
            Color32::from_rgb(175, 30, 40)
        };
        context.set_visuals_of(theme, visuals);
    }
    context.set_theme(egui::ThemePreference::System);
    context.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.slider_width = 150.0;
        for (text_style, size) in [
            (egui::TextStyle::Body, 15.0),
            (egui::TextStyle::Button, 15.0),
            (egui::TextStyle::Small, 12.0),
            (egui::TextStyle::Monospace, 13.0),
            (egui::TextStyle::Heading, 20.0),
        ] {
            style
                .text_styles
                .get_mut(&text_style)
                .expect("default text style")
                .size = size;
        }
    });
}

pub fn duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

pub fn or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}
