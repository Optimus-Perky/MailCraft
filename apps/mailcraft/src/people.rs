//! Suggested people: as a name or address is typed into To, Cc or Bcc, the people the account
//! has written to or heard from are offered (Settings > Privacy and data), best first.

use egui::{Key, Modifiers, RichText, vec2};
use wordcraft_ui_egui::theme::{Tokens, regular, semibold};

#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub name: String,
    pub addr: String,
    /// Higher is offered first: people written to count most, then how often and how lately.
    pub score: f64,
}

impl Person {
    /// As it goes in the field: "Name <addr>" (commas taken out of the name: they split addresses).
    pub fn entry(&self) -> String {
        let name = self.name.replace([',', ';', '<', '>', '"'], " ").split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() || name.eq_ignore_ascii_case(&self.addr) { self.addr.clone() } else { format!("{name} <{}>", self.addr) }
    }
}

/// Everyone to offer: senders (address, name, messages, newest date) and the addresses written
/// to (with how many times), less the account itself.
pub fn gather(me: &str, senders: &[(String, Option<String>, i64, Option<String>)], written: &[(String, i64)]) -> Vec<Person> {
    let now = chrono::Utc::now();
    let mut out: Vec<Person> = vec![];
    let age = |d: &Option<String>| {
        d.as_deref()
            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
            .map(|t| (now - t.with_timezone(&chrono::Utc)).num_days().max(0) as f64)
            .unwrap_or(365.0)
    };
    for (addr, name, n, last) in senders {
        out.push(Person { name: name.clone().unwrap_or_default(), addr: addr.clone(), score: *n as f64 + 30.0 / (1.0 + age(last) / 7.0) });
    }
    for (addr, n) in written {
        match out.iter_mut().find(|p| p.addr.eq_ignore_ascii_case(addr)) {
            Some(p) => p.score += 3.0 * *n as f64 + 20.0,
            None => out.push(Person { name: String::new(), addr: addr.clone(), score: 3.0 * *n as f64 + 20.0 }),
        }
    }
    out.retain(|p| !p.addr.eq_ignore_ascii_case(me) && p.addr.contains('@'));
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// The text before the address being typed, and that address so far.
fn split_last(text: &str) -> (&str, &str) {
    match text.rfind([',', ';']) {
        Some(i) => (text.get(..=i).unwrap_or(""), text.get(i + 1..).unwrap_or("")),
        None => ("", text),
    }
}

/// Who matches what is being typed (the start of a word of the name, or of the address).
pub fn matches<'a>(people: &'a [Person], text: &str, hidden: &[String], max: usize) -> Vec<&'a Person> {
    let (before, typed) = split_last(text);
    let q = typed.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let already = before.to_lowercase();
    people
        .iter()
        .filter(|p| !hidden.iter().any(|h| h.eq_ignore_ascii_case(&p.addr)) && !already.contains(&p.addr.to_lowercase()))
        .filter(|p| {
            let addr = p.addr.to_lowercase();
            let name = p.name.to_lowercase();
            addr.starts_with(&q)
                || name.starts_with(&q)
                || name.split_whitespace().any(|w| w.starts_with(&q))
                || addr.split(['.', '@', '_', '-']).any(|w| w.starts_with(&q))
        })
        .take(max)
        .collect()
}

/// The text with the address being typed replaced by the chosen person.
pub fn accept(text: &str, p: &Person) -> String {
    let (before, _) = split_last(text);
    let before = before.trim_end();
    if before.is_empty() { format!("{}, ", p.entry()) } else { format!("{before} {}, ", p.entry()) }
}

/// A small close cross, drawn (not every font has the character).
pub fn cross_button(ui: &mut egui::Ui, t: &Tokens) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(20.0, 20.0), egui::Sense::click());
    if r.hovered() {
        ui.painter().rect_filled(rect, 3.0, t.hover);
    }
    let c = rect.center();
    let s = egui::Stroke::new(1.3, if r.hovered() { t.text } else { t.text_dim });
    ui.painter().line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], s);
    ui.painter().line_segment([c + vec2(4.0, -4.0), c + vec2(-4.0, 4.0)], s);
    r
}

/// What happened in a recipient field this frame.
#[derive(Default)]
pub struct FieldOut {
    /// The person asked for this address not to be suggested again.
    pub hide: Option<String>,
}

/// A To / Cc / Bcc box that suggests people while typing. Up and Down choose, Enter or Tab
/// takes the chosen one, Esc closes the list.
pub fn recipient_field(
    ui: &mut egui::Ui,
    id: &str,
    text: &mut String,
    people: &[Person],
    hidden: &[String],
    width: f32,
    hint: &str,
    t: &Tokens,
) -> FieldOut {
    let mut out = FieldOut::default();
    let edit_id = egui::Id::new(("recipients", id));
    let pick_id = edit_id.with("pick");
    let area_id = edit_id.with("area");
    let found = matches(people, text, hidden, 6);
    let had_focus = ui.memory(|m| m.has_focus(edit_id));
    let closed = ui.data(|d| d.get_temp::<String>(edit_id.with("closed"))).is_some_and(|c| c == *text);
    let open = had_focus && !found.is_empty() && !closed;
    let mut pick = ui.data(|d| d.get_temp::<usize>(pick_id)).unwrap_or(0).min(found.len().saturating_sub(1));
    let mut take: Option<&Person> = None;
    // Keys the list uses, taken before the box sees them (Tab would move on, Enter would leave).
    if open {
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                pick = (pick + 1).min(found.len() - 1);
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                pick = pick.saturating_sub(1);
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                take = found.get(pick).copied();
            }
        });
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            ui.data_mut(|d| d.insert_temp(edit_id.with("closed"), text.clone()));
        }
    }
    let resp = ui.add(egui::TextEdit::singleline(text).id(edit_id).desired_width(width).hint_text(hint));
    if open && take.is_none() {
        let shown =
            egui::Area::new(area_id).order(egui::Order::Foreground).fixed_pos(resp.rect.left_bottom() + vec2(0.0, 2.0)).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(resp.rect.width().min(460.0));
                    ui.label(RichText::new("Suggested people").font(regular(11.0)).color(t.text_dim));
                    for (k, p) in found.iter().enumerate() {
                        let row = ui.horizontal(|ui| {
                            let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width() - 28.0, 34.0), egui::Sense::click());
                            if k == pick || r.hovered() {
                                ui.painter().rect_filled(rect, 3.0, if k == pick { t.checked } else { t.hover });
                            }
                            let top = if p.name.is_empty() { p.addr.clone() } else { p.name.clone() };
                            ui.painter().text(rect.left_top() + vec2(8.0, 3.0), egui::Align2::LEFT_TOP, top, semibold(12.5), t.text);
                            ui.painter().text(rect.left_top() + vec2(8.0, 18.0), egui::Align2::LEFT_TOP, &p.addr, regular(11.5), t.text_dim);
                            let x = cross_button(ui, t).on_hover_text("Don't suggest this person");
                            (r, x)
                        });
                        let (r, x) = row.inner;
                        if x.clicked() {
                            out.hide = Some(p.addr.clone());
                        } else if r.clicked() {
                            take = Some(p);
                        }
                    }
                });
            });
        let _ = shown;
    }
    if let Some(p) = take {
        *text = accept(text, p);
        pick = 0;
        // Back in the box, at the end, ready for the next address.
        ui.memory_mut(|m| m.request_focus(edit_id));
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), edit_id) {
            let end = egui::text::CCursor::new(text.chars().count());
            st.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
            st.store(ui.ctx(), edit_id);
        }
    }
    ui.data_mut(|d| d.insert_temp(pick_id, pick));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_and_fills_in() {
        let senders = vec![
            ("ann@example.org".to_string(), Some("Ann Lee".to_string()), 4, None),
            ("bob@example.org".to_string(), Some("Bob, Jones".to_string()), 1, None),
            ("me@example.com".to_string(), Some("Me".to_string()), 9, None),
        ];
        let people = gather("me@example.com", &senders, &[("bob@example.org".to_string(), 2), ("carol@work.example".to_string(), 1)]);
        assert!(people.iter().all(|p| p.addr != "me@example.com"));
        // Written to beats heard from.
        assert_eq!(people.first().map(|p| p.addr.as_str()), Some("bob@example.org"));
        assert_eq!(matches(&people, "le", &[], 5).iter().map(|p| p.addr.as_str()).collect::<Vec<_>>(), vec!["ann@example.org"]);
        assert_eq!(matches(&people, "work", &[], 5).len(), 1);
        assert!(matches(&people, "ann@example.org, an", &[], 5).is_empty(), "already there");
        assert!(matches(&people, "ann", &["ANN@example.org".into()], 5).is_empty(), "hidden");
        let bob = people.iter().find(|p| p.addr.starts_with("bob")).unwrap();
        assert_eq!(bob.entry(), "Bob Jones <bob@example.org>");
        assert_eq!(accept("ann@example.org; bo", bob), "ann@example.org; Bob Jones <bob@example.org>, ");
        assert_eq!(accept("bo", bob), "Bob Jones <bob@example.org>, ");
    }
}
