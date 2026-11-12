//! Meeting invitations in mail (iMIP, RFC 6047): a message carrying a text/calendar part with
//! METHOD REQUEST shows a card above the text with Accept, Tentative and Decline. The answer goes
//! into a calendar of the person's choosing and back to the organiser as a REPLY (so Outlook,
//! Gmail and the rest record it). A CANCEL offers to take the meeting out of the calendar; a
//! REPLY to one of the person's own meetings offers to record the attendee's answer.

use egui::{RichText, vec2};
use mail_parser::MimeHeaders;
use mailcraft_core::calstore::{CalRow, EventRow};
use mailcraft_core::ical::{self, Event, When};
use wordcraft_ui_egui::theme::{Tokens, regular, semibold};

use crate::MailApp;
use crate::calendar::{CalRequest, Itip, SourceKind};

/// An invitation (or cancellation, or reply) found in the open message.
pub struct Invite {
    /// REQUEST, CANCEL, REPLY (PUBLISH and others are shown, not answered).
    pub method: String,
    pub event: Event,
    /// Copies of the meeting already in a calendar.
    pub existing: Vec<EventRow>,
    /// The calendar to put it in.
    pub calendar: Option<i64>,
    pub send_reply: bool,
    /// What was done, to show on the card.
    pub done: Option<String>,
}

pub enum Action {
    Answer(&'static str),
    Remove,
    RecordReply,
}

/// The invitation in a message, if it carries one.
pub fn find(m: &mail_parser::Message) -> Option<(String, Event)> {
    for p in &m.parts {
        let is_cal = p
            .content_type()
            .is_some_and(|c| c.ctype().eq_ignore_ascii_case("text") && c.subtype().is_some_and(|s| s.eq_ignore_ascii_case("calendar")));
        let is_ics = p.attachment_name().is_some_and(|n| n.to_ascii_lowercase().ends_with(".ics"));
        if !is_cal && !is_ics {
            continue;
        }
        let text = String::from_utf8_lossy(p.contents()).to_string();
        let events = ical::parse(&text);
        let Some(e) = events.iter().find(|e| e.recurrence_id.is_none()).or(events.first()).cloned() else { continue };
        let method = ical::method(&text).unwrap_or_else(|| "PUBLISH".into());
        return Some((method, e));
    }
    None
}

pub(crate) fn when(e: &Event) -> String {
    let tz = ical::local_zone();
    match (e.start, e.end) {
        (When::Date(a), When::Date(b)) => {
            let last = if b > a { b.pred_opt().unwrap_or(b) } else { a };
            if last == a {
                a.format("%A %-d %B %Y, all day").to_string()
            } else {
                format!("{} to {}, all day", a.format("%a %-d %b"), last.format("%a %-d %b %Y"))
            }
        }
        (s, en) => {
            let (s, en) = (s.utc(tz).with_timezone(&tz), en.utc(tz).with_timezone(&tz));
            if s.date_naive() == en.date_naive() {
                format!("{}, {} to {}", s.format("%A %-d %B %Y"), s.format("%H:%M"), en.format("%H:%M"))
            } else {
                format!("{} to {}", s.format("%a %-d %b %Y %H:%M"), en.format("%a %-d %b %Y %H:%M"))
            }
        }
    }
}

fn person(a: &ical::Attendee) -> String {
    if a.name.is_empty() { a.email.clone() } else { format!("{} <{}>", a.name, a.email) }
}

/// My answer in a stored copy, if any.
fn my_answer(rows: &[EventRow], me: &str) -> Option<String> {
    rows.iter().find_map(|r| {
        ical::parse(&r.ics).into_iter().find_map(|e| e.attendees.into_iter().find(|a| a.email.eq_ignore_ascii_case(me)).map(|a| a.partstat))
    })
}

/// The card above the message. `cals`: the calendars it can go in (not read-only).
pub fn card(ui: &mut egui::Ui, t: &Tokens, inv: &mut Invite, cals: &[(CalRow, String)], me: &str) -> Option<Action> {
    let mut act = None;
    let e = &inv.event;
    let heading = match inv.method.as_str() {
        "CANCEL" => format!("Meeting cancelled: {}", e.summary),
        "REPLY" => {
            let who = e.attendees.first();
            let what = match who.map(|a| a.partstat.as_str()) {
                Some("ACCEPTED") => "accepted",
                Some("DECLINED") => "declined",
                Some("TENTATIVE") => "tentatively accepted",
                _ => "replied to",
            };
            format!("{} {what}: {}", who.map(person).unwrap_or_else(|| "Someone".into()), e.summary)
        }
        _ => format!("Meeting invitation: {}", e.summary),
    };
    egui::Frame::new().fill(t.canvas).stroke(egui::Stroke::new(1.0, t.accent)).corner_radius(6.0).inner_margin(egui::Margin::symmetric(12, 10)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(heading).font(semibold(14.0)).color(t.text));
            ui.label(RichText::new(when(e)).font(regular(12.5)).color(t.text));
            if !e.location.is_empty() {
                ui.label(RichText::new(format!("Where: {}", e.location)).font(regular(12.0)).color(t.text_dim));
            }
            if let Some(o) = &e.organizer {
                ui.label(RichText::new(format!("Organiser: {}", person(o))).font(regular(12.0)).color(t.text_dim));
            }
            if inv.method == "REQUEST" && e.attendees.len() > 1 {
                let names: Vec<String> =
                    e.attendees.iter().take(8).map(|a| if a.name.is_empty() { a.email.clone() } else { a.name.clone() }).collect();
                let more = e.attendees.len().saturating_sub(8);
                let more = if more > 0 { format!(" and {more} more") } else { String::new() };
                ui.label(RichText::new(format!("Invited: {}{more}", names.join(", "))).font(regular(12.0)).color(t.text_dim));
            }
            let in_cal: Vec<String> =
                inv.existing.iter().filter_map(|r| cals.iter().find(|(c, _)| c.id == r.calendar_id).map(|(_, n)| n.clone())).collect();
            if let Some(d) = &inv.done {
                ui.label(RichText::new(d).font(semibold(12.0)).color(t.green));
            } else if !in_cal.is_empty() {
                let answer = match my_answer(&inv.existing, me).as_deref() {
                    Some("ACCEPTED") => " (you accepted)",
                    Some("TENTATIVE") => " (you said maybe)",
                    Some("DECLINED") => " (you declined)",
                    _ => "",
                };
                ui.label(RichText::new(format!("In your calendar: {}{answer}", in_cal.join(", "))).font(regular(12.0)).color(t.green));
            }
            ui.add_space(4.0);
            match inv.method.as_str() {
                "REQUEST" | "PUBLISH" if e.organizer.is_some() => {
                    ui.horizontal(|ui| {
                        if inv.existing.is_empty() {
                            ui.label(RichText::new("Add to").font(regular(12.0)).color(t.text_dim));
                            let shown = inv
                                .calendar
                                .and_then(|id| cals.iter().find(|(c, _)| c.id == id))
                                .map(|(_, n)| n.clone())
                                .unwrap_or_else(|| "(no calendar)".into());
                            egui::ComboBox::from_id_salt("invite_cal").width(260.0).selected_text(shown).show_ui(ui, |ui| {
                                for (c, n) in cals {
                                    ui.selectable_value(&mut inv.calendar, Some(c.id), n);
                                }
                            });
                        }
                        ui.checkbox(&mut inv.send_reply, "Send my answer to the organiser");
                    });
                    ui.horizontal(|ui| {
                        let can_add = inv.calendar.is_some() || !inv.existing.is_empty();
                        for (label, ps) in [("Accept", "ACCEPTED"), ("Tentative", "TENTATIVE"), ("Decline", "DECLINED")] {
                            let enabled = ps == "DECLINED" || can_add;
                            if ui
                                .add_enabled(enabled, egui::Button::new(RichText::new(label).font(semibold(12.5))).min_size(vec2(84.0, 26.0)))
                                .clicked()
                            {
                                act = Some(Action::Answer(ps));
                            }
                        }
                        if cals.is_empty() {
                            ui.label(RichText::new("Add a calendar account to keep meetings in it.").font(regular(11.5)).color(t.text_dim));
                        }
                    });
                }
                "CANCEL" if !inv.existing.is_empty() && ui.button("Remove from my calendar").clicked() => act = Some(Action::Remove),
                "REPLY" if !inv.existing.is_empty() && ui.button("Record the answer in my calendar").clicked() => act = Some(Action::RecordReply),
                _ => {}
            }
        },
    );
    act
}

impl MailApp {
    /// Calendars an invitation can go in (writable, of set-up accounts), with their names as
    /// "Calendar · account".
    pub(crate) fn invite_calendars(&self) -> Vec<(CalRow, String)> {
        let Some(store) = &self.store else { return vec![] };
        let sources = &self.cal.sources;
        store
            .calendars()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !c.read_only)
            .filter_map(|c| {
                let src = sources.iter().find(|s| s.id == c.source)?;
                let name = format!("{} \u{00b7} {}", c.name, src.label);
                Some((c, name))
            })
            .collect()
    }

    /// The invitation found in a message just opened: what is already in the calendars, and
    /// where a new one would go (the calendar used last time, else the first).
    pub(crate) fn invite_for(&self, method: String, event: Event) -> Invite {
        let existing = self.store.as_ref().and_then(|s| s.events_with_uid(&event.uid).ok()).unwrap_or_default();
        let cals = self.invite_calendars();
        let calendar = self.invite_last.filter(|id| cals.iter().any(|(c, _)| c.id == *id)).or_else(|| cals.first().map(|(c, _)| c.id));
        Invite { method, event, existing, calendar, send_reply: true, done: None }
    }

    pub(crate) fn invite_act(&mut self, act: Action) {
        let Some(acct) = self.current().cloned() else { return };
        let Some(inv) = self.open.as_ref().and_then(|o| o.invite.as_ref()) else { return };
        let (event, existing, calendar, send_reply) = (inv.event.clone(), inv.existing.clone(), inv.calendar, inv.send_reply);
        let me = acct.email.clone();
        let display = self.settings.name_for(&acct.email).filter(|n| !n.trim().is_empty());
        let name = display.clone().unwrap_or_else(|| acct.email.clone());
        let mut errors: Vec<String> = vec![];
        let done = match act {
            Action::Answer(partstat) => {
                // My answer, to go back to the organiser (mailed unless the calendar server sends it).
                let itip = event.organizer.as_ref().filter(|o| send_reply && !o.email.is_empty()).map(|o| Itip {
                    method: "REPLY".into(),
                    event: ical::reply(&event, &me, &name, partstat),
                    to: vec![o.email.clone()],
                    from: me.clone(),
                });
                let mut itip = itip;
                if partstat == "DECLINED" {
                    // Out of the calendar (the server may tell the organiser as it does).
                    for r in &existing {
                        let it: Vec<Itip> = itip.take().into_iter().collect();
                        if let Err(e) = self.cal_send(r.calendar_id, |src, pw| CalRequest::Delete(src, pw, r.id, it)) {
                            errors.push(e);
                        }
                    }
                } else {
                    let target = existing.first().map(|r| (r.calendar_id, Some(r.id))).or(calendar.map(|c| (c, None)));
                    if let Some((cal, row)) = target {
                        let mut mine = ical::with_my_answer(&event, &me, &name, partstat);
                        // Microsoft makes a meeting with attendees into an invitation from you to
                        // them all: keep your copy to yourself, naming the organiser in the notes.
                        if self.source_kind_of(cal) == Some(SourceKind::Microsoft) {
                            if let Some(o) = &mine.organizer {
                                let line = format!("Organiser: {}", person(o));
                                mine.description = if mine.description.is_empty() { line } else { format!("{line}\n\n{}", mine.description) };
                            }
                            mine.attendees.clear();
                            mine.organizer = None;
                        }
                        self.invite_last = Some(cal);
                        let it: Vec<Itip> = itip.take().into_iter().collect();
                        if let Err(e) = self.cal_send(cal, |src, pw| CalRequest::Save(src, pw, cal, row, Box::new(mine), it)) {
                            errors.push(e);
                        }
                    }
                }
                // Nothing in a calendar to go with it: the answer goes straight out.
                if let Some(it) = itip {
                    self.send_itip(&it);
                }
                match partstat {
                    "ACCEPTED" => "You accepted.",
                    "TENTATIVE" => "You said maybe.",
                    _ => "You declined.",
                }
                .to_string()
                    + if send_reply && event.organizer.is_some() { " Your answer is being sent to the organiser." } else { "" }
            }
            Action::Remove => {
                for r in &existing {
                    if let Err(e) = self.cal_send(r.calendar_id, |src, pw| CalRequest::Delete(src, pw, r.id, vec![])) {
                        errors.push(e);
                    }
                }
                "Removed from your calendar.".to_string()
            }
            Action::RecordReply => {
                let Some(who) = event.attendees.first().cloned() else { return };
                for r in &existing {
                    let Some(mut mine) = ical::parse(&r.ics).into_iter().find(|e| e.recurrence_id.is_none()) else { continue };
                    match mine.attendees.iter_mut().find(|a| a.email.eq_ignore_ascii_case(&who.email)) {
                        Some(a) => a.partstat = who.partstat.clone(),
                        None => mine.attendees.push(who.clone()),
                    }
                    let (cal, row) = (r.calendar_id, r.id);
                    if let Err(e) = self.cal_send(cal, |src, pw| CalRequest::Save(src, pw, cal, Some(row), Box::new(mine), vec![])) {
                        errors.push(e);
                    }
                }
                format!("Recorded {}'s answer.", if who.name.is_empty() { who.email.clone() } else { who.name.clone() })
            }
        };
        if let Some(inv) = self.open.as_mut().and_then(|o| o.invite.as_mut()) {
            inv.done = Some(if errors.is_empty() { done } else { format!("{done} But: {}", errors.join("; ")) });
        }
        self.cal.dirty = true;
    }
}
