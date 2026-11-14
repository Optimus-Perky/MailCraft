//! Lines of the message header (subject, from, to, date): text you can select with the mouse and
//! copy with Ctrl+C or from a right-click menu.
//!
//! They are read-only `TextEdit`s, not labels: egui starts a new selection on ANY mouse press, the
//! right button included, so a right-click wiped the selection and labels have no menu at all. Here
//! the selection from before a right-click is put back, and the menu copies it.

use egui::{Color32, FontId, Id, PointerButton, TextEdit, Ui, text_edit::TextEditState};

/// The part of `text` between two char positions (either order), never panicking on odd input.
pub fn char_slice(text: &str, a: usize, b: usize) -> String {
    let (from, to) = if a <= b { (a, b) } else { (b, a) };
    text.chars().skip(from).take(to.saturating_sub(from)).collect()
}

/// What is selected in the field `id`, if anything.
fn selected(ui: &Ui, id: Id, text: &str) -> Option<String> {
    let state = TextEditState::load(ui.ctx(), id)?;
    let range = state.cursor.char_range()?;
    let s = char_slice(text, range.primary.index.into(), range.secondary.index.into());
    (!s.is_empty()).then_some(s)
}

/// Is the right-click menu of the header line `id` open? (egui keeps one popup open at a time.)
pub fn menu_open(ctx: &egui::Context, id: Id) -> bool {
    egui::Popup::is_id_open(ctx, id.with("popup"))
}

/// One header line. Wraps like a label; `id` must be unique per message and field.
pub fn line(ui: &mut Ui, id: Id, text: &str, font: FontId, color: Color32) {
    let right_press = ui.input(|i| i.pointer.button_pressed(PointerButton::Secondary));
    let before = if right_press { TextEditState::load(ui.ctx(), id) } else { None };
    let mut shown = text;
    let resp = TextEdit::multiline(&mut shown)
        .id(id)
        .font(font)
        .text_color(color)
        .frame(egui::Frame::NONE)
        .margin(egui::Margin::ZERO)
        .desired_rows(1)
        .desired_width(f32::INFINITY)
        .show(ui)
        .response;
    if right_press
        && resp.hovered()
        && let Some(state) = before
    {
        state.store(ui.ctx(), id);
    }
    // The menu opens ABOVE the pointer, so it always sits over the header (egui), never over the
    // native mail view below it: that view then needn't hide while the menu is open (see menu_open).
    egui::Popup::context_menu(&resp).align(egui::RectAlign::TOP_START).align_alternatives(&[egui::RectAlign::TOP_END]).show(|ui| {
        // A "Select all" here could set the selection but egui would not paint it after the menu
        // closes, so the whole line gets its own copy item instead.
        if let Some(picked) = selected(ui, id, text)
            && ui.button("Copy").clicked()
        {
            ui.ctx().copy_text(picked);
            ui.close();
        }
        if ui.button("Copy line").clicked() {
            ui.ctx().copy_text(text.to_owned());
            ui.close();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::char_slice;

    #[test]
    fn slices_by_chars_in_either_order_and_clamps() {
        assert_eq!(char_slice("Re: café plans", 4, 8), "café");
        assert_eq!(char_slice("Re: café plans", 8, 4), "café");
        assert_eq!(char_slice("abc", 1, 99), "bc");
        assert_eq!(char_slice("abc", 5, 9), "");
        assert_eq!(char_slice("", 0, 3), "");
    }
}
