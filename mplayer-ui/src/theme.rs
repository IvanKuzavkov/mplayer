use eframe::egui::{self, Color32};

pub const ACCENT_DARK: Color32 = Color32::from_rgb(221, 190, 83);
pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(125, 91, 0);

pub fn accent(theme: &egui::Visuals) -> Color32 {
    if theme.dark_mode {
        ACCENT_DARK
    } else {
        ACCENT_LIGHT
    }
}

enum UIThemes {
    Dark(egui::style::Visuals),
    Light(egui::style::Visuals),
}

impl UIThemes {
    fn new_dark() -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(30, 31, 35);
        visuals.window_fill = visuals.panel_fill;
        visuals.extreme_bg_color = Color32::from_rgb(19, 20, 23);
        visuals.weak_text_color = Some(Color32::from_rgb(164, 166, 171));
        visuals.selection.bg_fill = Color32::from_rgb(80, 69, 34);
        visuals.selection.stroke = egui::Stroke::new(1.0, accent(&visuals));
        visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0, Color32::from_rgb(53, 54, 60));
        visuals.widgets.active.bg_fill = Color32::from_rgb(65, 57, 34);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(58, 59, 64);
        visuals.faint_bg_color = Color32::from_rgb(37, 38, 42);
        visuals.error_fg_color = Color32::LIGHT_RED;
        UIThemes::Dark(visuals)
    }

    fn new_light() -> Self {
        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = Color32::from_rgb(253, 252, 249);
        visuals.window_fill = visuals.panel_fill;
        visuals.extreme_bg_color = Color32::from_rgb(246, 245, 241);
        visuals.weak_text_color = Some(Color32::from_rgb(92, 94, 99));
        visuals.selection.bg_fill = Color32::from_rgb(244, 225, 166);
        visuals.selection.stroke = egui::Stroke::new(1.0, accent(&visuals));
        visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0, Color32::from_rgb(200, 199, 193));
        visuals.widgets.active.bg_fill = Color32::from_rgb(235, 216, 157);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(234, 233, 228);
        visuals.faint_bg_color = Color32::from_rgb(248, 247, 243);
        visuals.error_fg_color = Color32::from_rgb(175, 30, 40);
        UIThemes::Light(visuals)
    }

    pub fn from_egui_theme(theme: &egui::Theme) -> Self {
        if theme == &egui::Theme::Dark {
            Self::new_dark()
        } else {
            Self::new_light()
        }
    }

    pub fn into_visuals(self) -> egui::style::Visuals {
        match self {
            UIThemes::Dark(visuals) => visuals,
            UIThemes::Light(visuals) => visuals,
        }
    }
}

impl Into<egui::style::Visuals> for UIThemes {
    fn into(self) -> egui::Visuals {
        self.into_visuals()
    }
}

pub fn apply(context: &egui::Context) {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        let visuals = UIThemes::from_egui_theme(&theme);
        context.set_visuals_of(theme, visuals.into());
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
