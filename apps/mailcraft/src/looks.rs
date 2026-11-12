//! Themes: light, dark, and two high-contrast ones (after Windows' High Contrast Black and
//! White), or follow the system. Each gives the window's colours and how mail is shown in the
//! browser view.

use egui::Color32;
use serde::{Deserialize, Serialize};
use wordcraft_ui_egui::theme::Tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
    ContrastBlack,
    ContrastWhite,
}

impl Theme {
    /// The theme actually used, with "System" settled by the system's setting.
    pub fn resolve(self, system_dark: bool) -> Theme {
        match self {
            Theme::System if system_dark => Theme::Dark,
            Theme::System => Theme::Light,
            t => t,
        }
    }

    pub fn tokens(self) -> Tokens {
        match self {
            Theme::System | Theme::Light => Tokens::light(),
            Theme::Dark => Tokens::dark(),
            Theme::ContrastBlack => contrast_black(),
            Theme::ContrastWhite => contrast_white(),
        }
    }

    pub fn high_contrast(self) -> bool {
        matches!(self, Theme::ContrastBlack | Theme::ContrastWhite)
    }

    /// Dark reading: the message's own colours are changed (darkmail.rs), not filtered.
    pub fn dark_reading(self, dark_messages: bool) -> bool {
        self == Theme::Dark && dark_messages
    }

    /// Style added to a message in the browser view (after the message's own).
    pub fn message_css(self, dark_messages: bool) -> &'static str {
        let _ = dark_messages;
        match self {
            Theme::ContrastBlack => {
                "*,*::before,*::after{background-color:#000!important;color:#fff!important;border-color:#fff!important;\
                 text-shadow:none!important;box-shadow:none!important}\
                 html,body{background:#000!important}a,a *{color:#ff0!important;text-decoration:underline!important}\
                 ::selection{background:#1aebff!important;color:#000!important}"
            }
            Theme::ContrastWhite => {
                "*,*::before,*::after{background-color:#fff!important;color:#000!important;border-color:#000!important;\
                 text-shadow:none!important;box-shadow:none!important}\
                 html,body{background:#fff!important}a,a *{color:#00009f!important;text-decoration:underline!important}\
                 ::selection{background:#37006e!important;color:#fff!important}"
            }
            _ => "",
        }
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Windows' High Contrast Black: black ground, white text, yellow links, cyan selection,
/// green for what cannot be used.
fn contrast_black() -> Tokens {
    let white = rgb(0xFF, 0xFF, 0xFF);
    let black = rgb(0, 0, 0);
    Tokens {
        dark: true,
        title_bar: black,
        ribbon: black,
        tab_strip: black,
        canvas: black,
        panel: black,
        border: white,
        border_strong: white,
        text: white,
        text_dim: white,
        text_disabled: rgb(0x3F, 0xF2, 0x3F),
        icon: white,
        hover: rgb(0x26, 0x26, 0x26),
        pressed: rgb(0x40, 0x40, 0x40),
        checked: rgb(0x00, 0x4B, 0x5A),
        accent: rgb(0x1A, 0xEB, 0xFF),
        accent_text: rgb(0xFF, 0xFF, 0x00),
        input: black,
        input_border: white,
        group_label: white,
        ruler: black,
        ruler_margin: rgb(0x40, 0x40, 0x40),
        ruler_tick: white,
        status_bar: black,
        selection: Color32::from_rgba_unmultiplied(0x1A, 0xEB, 0xFF, 0x70),
        caret: white,
        page_shadow: Color32::TRANSPARENT,
        menu: black,
        blue: rgb(0xFF, 0xFF, 0x00),
        red: rgb(0xFF, 0x70, 0x70),
        green: rgb(0x3F, 0xF2, 0x3F),
        orange: rgb(0xFF, 0xB0, 0x00),
    }
}

/// Windows' High Contrast White: white ground, black text, dark blue links, purple selection.
fn contrast_white() -> Tokens {
    let white = rgb(0xFF, 0xFF, 0xFF);
    let black = rgb(0, 0, 0);
    Tokens {
        dark: false,
        title_bar: white,
        ribbon: white,
        tab_strip: white,
        canvas: white,
        panel: white,
        border: black,
        border_strong: black,
        text: black,
        text_dim: black,
        text_disabled: rgb(0x60, 0x00, 0x00),
        icon: black,
        hover: rgb(0xE0, 0xE0, 0xE0),
        pressed: rgb(0xC8, 0xC8, 0xC8),
        checked: rgb(0xE2, 0xD6, 0xFF),
        accent: rgb(0x37, 0x00, 0x6E),
        accent_text: rgb(0x00, 0x00, 0x9F),
        input: white,
        input_border: black,
        group_label: black,
        ruler: white,
        ruler_margin: rgb(0xC8, 0xC8, 0xC8),
        ruler_tick: black,
        status_bar: white,
        selection: Color32::from_rgba_unmultiplied(0x37, 0x00, 0x6E, 0x50),
        caret: black,
        page_shadow: Color32::TRANSPARENT,
        menu: white,
        blue: rgb(0x00, 0x00, 0x9F),
        red: rgb(0xB0, 0x00, 0x00),
        green: rgb(0x00, 0x60, 0x00),
        orange: rgb(0x80, 0x30, 0x00),
    }
}

/// After `theme::apply`: thicker outlines in high contrast, so every control's edge shows.
pub fn strengthen(ctx: &egui::Context, t: &Tokens) {
    ctx.global_style_mut(|s| {
        let w = &mut s.visuals.widgets;
        let edge = egui::Stroke::new(1.5, t.text);
        w.noninteractive.bg_stroke = egui::Stroke::new(1.0, t.text);
        w.inactive.bg_stroke = edge;
        w.hovered.bg_stroke = egui::Stroke::new(2.0, t.accent);
        w.active.bg_stroke = egui::Stroke::new(2.0, t.accent);
        s.visuals.selection.stroke = egui::Stroke::new(2.0, t.accent);
    });
}
