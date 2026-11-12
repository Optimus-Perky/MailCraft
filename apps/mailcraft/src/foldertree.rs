//! The folder pane as a tree, the way Betterbird/Thunderbird show it: special folders first
//! (Inbox, Drafts, Sent, Archive, Junk, Trash), then the rest by name, subfolders nested under
//! their parents and collapsible. Icons are drawn here in code.

use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2, vec2};
use mailcraft_core::FolderRow;
use std::collections::HashSet;

/// One visible row.
pub struct Row {
    /// The folder's full name on the server (also the collapse key).
    pub full: String,
    pub label: String,
    pub depth: usize,
    /// Index into the folder list; None for a parent the server only implies (e.g. "[Gmail]").
    pub folder: Option<usize>,
    pub has_children: bool,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
    Folder,
}

impl Kind {
    fn rank(self) -> u8 {
        match self {
            Kind::Inbox => 0,
            Kind::Drafts => 1,
            Kind::Sent => 2,
            Kind::Archive => 3,
            Kind::Junk => 4,
            Kind::Trash => 5,
            Kind::Folder => 6,
        }
    }
}

/// What a folder is: by its special-use mark, else by the usual names.
pub fn kind(f: &FolderRow) -> Kind {
    match f.special.as_deref() {
        Some("\\Inbox") => return Kind::Inbox,
        Some("\\Drafts") => return Kind::Drafts,
        Some("\\Sent") => return Kind::Sent,
        Some("\\Archive") => return Kind::Archive,
        Some("\\Junk") => return Kind::Junk,
        Some("\\Trash") => return Kind::Trash,
        _ => {}
    }
    let last = leaf(&f.name, f.delimiter.as_deref()).to_ascii_lowercase();
    if f.name.eq_ignore_ascii_case("INBOX") {
        return Kind::Inbox;
    }
    match last.as_str() {
        "drafts" | "draft" => Kind::Drafts,
        "sent" | "sent items" | "sent messages" | "sent mail" => Kind::Sent,
        "archive" | "archives" => Kind::Archive,
        "junk" | "junk e-mail" | "junk email" | "spam" => Kind::Junk,
        "trash" | "deleted items" | "deleted messages" | "bin" => Kind::Trash,
        _ => Kind::Folder,
    }
}

fn leaf<'a>(name: &'a str, delim: Option<&str>) -> &'a str {
    match delim.filter(|d| !d.is_empty()) {
        Some(d) => name.rsplit(d).next().unwrap_or(name),
        None => name,
    }
}

#[derive(Default)]
struct Node {
    seg: String,
    full: String,
    folder: Option<usize>,
    kind: Option<Kind>,
    children: Vec<Node>,
}

impl Node {
    fn kind(&self) -> Kind {
        self.kind.unwrap_or(Kind::Folder)
    }
}

/// The visible rows for these folders, with `collapsed` folders' children hidden.
pub fn rows(folders: &[FolderRow], collapsed: &HashSet<String>) -> Vec<Row> {
    let mut root = Node::default();
    for (i, f) in folders.iter().enumerate() {
        let d = f.delimiter.as_deref().filter(|d| !d.is_empty());
        let parts: Vec<&str> = match d {
            Some(d) => f.name.split(d).collect(),
            None => vec![f.name.as_str()],
        };
        let mut node = &mut root;
        let mut full = String::new();
        for (n, seg) in parts.iter().enumerate() {
            if n > 0 {
                full.push_str(d.unwrap_or(""));
            }
            full.push_str(seg);
            let at = match node.children.iter().position(|c| c.seg == *seg) {
                Some(at) => at,
                None => {
                    node.children.push(Node { seg: seg.to_string(), full: full.clone(), ..Default::default() });
                    node.children.len() - 1
                }
            };
            node = &mut node.children[at];
        }
        node.folder = Some(i);
        node.kind = Some(kind(f));
    }
    let mut out = vec![];
    flatten(&mut root, 0, collapsed, &mut out);
    out
}

fn flatten(node: &mut Node, depth: usize, collapsed: &HashSet<String>, out: &mut Vec<Row>) {
    node.children.sort_by(|a, b| a.kind().rank().cmp(&b.kind().rank()).then_with(|| a.seg.to_lowercase().cmp(&b.seg.to_lowercase())));
    for c in &mut node.children {
        let label = if c.kind() == Kind::Inbox && depth == 0 { "Inbox".to_string() } else { c.seg.clone() };
        out.push(Row { full: c.full.clone(), label, depth, folder: c.folder, has_children: !c.children.is_empty(), kind: c.kind() });
        if !collapsed.contains(&c.full) {
            flatten(c, depth + 1, collapsed, out);
        }
    }
}

/// A small triangle: pointing right when closed, down when open.
pub fn twisty(p: &Painter, center: Pos2, open: bool, c: Color32) {
    let pts = if open {
        vec![center + vec2(-3.5, -1.8), center + vec2(3.5, -1.8), center + vec2(0.0, 2.5)]
    } else {
        vec![center + vec2(-1.8, -3.5), center + vec2(2.5, 0.0), center + vec2(-1.8, 3.5)]
    };
    p.add(egui::Shape::convex_polygon(pts, c, Stroke::NONE));
}

pub const FOLDER_FILL: Color32 = Color32::from_rgb(0xF2, 0xC9, 0x5C);
const FOLDER_EDGE: Color32 = Color32::from_rgb(0xC4, 0x96, 0x2C);

/// Paint a 16 x 16 icon at `r` (its top-left is used). `c` is the line colour.
pub fn icon(p: &Painter, r: Rect, kind: Option<Kind>, c: Color32, accent: Color32) {
    icon_tinted(p, r, kind, c, accent, None);
}

/// The same, in a folder colour the person chose (Betterbird-style coloured folders).
pub fn icon_tinted(p: &Painter, r: Rect, kind: Option<Kind>, c: Color32, accent: Color32, tint: Option<Color32>) {
    let accent = tint.unwrap_or(accent);
    let (fill, edge) = match tint {
        Some(t) => (t, t.gamma_multiply(0.75)),
        None => (FOLDER_FILL, FOLDER_EDGE),
    };
    let o = r.min;
    let at = |x: f32, y: f32| pos2(o.x + x, o.y + y);
    let s = Stroke::new(1.3, c);
    let a = Stroke::new(1.3, accent);
    let line = |pts: &[(f32, f32)], st: Stroke| p.add(egui::Shape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), st));
    match kind {
        // The account: an envelope.
        None => {
            p.rect_stroke(Rect::from_min_max(at(1.5, 3.5), at(14.5, 12.5)), 1.0, a, egui::StrokeKind::Middle);
            line(&[(1.5, 4.0), (8.0, 9.0), (14.5, 4.0)], a);
        }
        Some(Kind::Inbox) => {
            line(&[(1.5, 9.0), (1.5, 14.0), (14.5, 14.0), (14.5, 9.0)], a);
            line(&[(1.5, 9.0), (5.0, 9.0), (6.0, 11.0), (10.0, 11.0), (11.0, 9.0), (14.5, 9.0)], a);
            line(&[(8.0, 1.5), (8.0, 7.5)], a);
            line(&[(5.5, 5.0), (8.0, 7.5), (10.5, 5.0)], a);
        }
        Some(Kind::Drafts) => {
            let s = tint.map(|t| Stroke::new(1.3, t)).unwrap_or(s);
            line(&[(3.0, 1.5), (10.0, 1.5), (13.0, 4.5), (13.0, 14.5), (3.0, 14.5), (3.0, 1.5)], s);
            line(&[(6.0, 12.0), (12.5, 5.5)], a);
            line(&[(5.5, 12.5), (6.0, 12.0)], Stroke::new(2.2, accent));
        }
        Some(Kind::Sent) => {
            line(&[(1.5, 7.5), (14.5, 1.5), (10.0, 14.5), (7.5, 9.0), (1.5, 7.5)], a);
            line(&[(7.5, 9.0), (14.5, 1.5)], a);
        }
        Some(Kind::Archive) => {
            let s = tint.map(|t| Stroke::new(1.3, t)).unwrap_or(s);
            p.rect_stroke(Rect::from_min_max(at(1.5, 2.0), at(14.5, 5.5)), 1.0, s, egui::StrokeKind::Middle);
            line(&[(2.5, 5.5), (2.5, 14.0), (13.5, 14.0), (13.5, 5.5)], s);
            line(&[(6.0, 8.5), (10.0, 8.5)], a);
        }
        Some(Kind::Junk) => {
            let orange = tint.unwrap_or(Color32::from_rgb(0xE0, 0x7B, 0x1F));
            p.circle_stroke(at(8.0, 8.0), 6.0, Stroke::new(1.4, orange));
            line(&[(3.8, 12.2), (12.2, 3.8)], Stroke::new(1.4, orange));
        }
        Some(Kind::Trash) => {
            let s = tint.map(|t| Stroke::new(1.3, t)).unwrap_or(s);
            line(&[(2.0, 4.0), (14.0, 4.0)], s);
            line(&[(6.0, 4.0), (6.5, 2.0), (9.5, 2.0), (10.0, 4.0)], s);
            line(&[(3.5, 4.0), (4.5, 14.5), (11.5, 14.5), (12.5, 4.0)], s);
            line(&[(6.5, 6.5), (6.8, 12.5)], s);
            line(&[(9.5, 6.5), (9.2, 12.5)], s);
        }
        Some(Kind::Folder) => {
            let body = vec![at(1.5, 4.5), at(6.0, 4.5), at(7.5, 6.0), at(14.5, 6.0), at(14.5, 13.5), at(1.5, 13.5)];
            p.add(egui::Shape::convex_polygon(body.clone(), fill, Stroke::NONE));
            p.add(egui::Shape::closed_line(body, Stroke::new(1.0, edge)));
            p.add(egui::Shape::convex_polygon(vec![at(1.5, 4.5), at(1.5, 3.0), at(5.5, 3.0), at(6.0, 4.5)], edge, Stroke::NONE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, special: Option<&str>) -> FolderRow {
        FolderRow {
            id: 0,
            account: "a".into(),
            name: name.into(),
            delimiter: Some("/".into()),
            special: special.map(str::to_string),
            uidvalidity: None,
            total: 0,
            unread: 0,
        }
    }

    #[test]
    fn nests_and_orders_like_betterbird() {
        let folders = vec![
            f("Zebra", None),
            f("[Gmail]/Trash", Some("\\Trash")),
            f("INBOX", Some("\\Inbox")),
            f("[Gmail]/Sent Mail", Some("\\Sent")),
            f("Receipts/2026", None),
            f("Receipts", None),
            f("Drafts", Some("\\Drafts")),
        ];
        let r = rows(&folders, &HashSet::new());
        let shown: Vec<(usize, &str, bool)> = r.iter().map(|r| (r.depth, r.label.as_str(), r.folder.is_some())).collect();
        assert_eq!(
            shown,
            vec![
                (0, "Inbox", true),
                (0, "Drafts", true),
                (0, "[Gmail]", false),
                (1, "Sent Mail", true),
                (1, "Trash", true),
                (0, "Receipts", true),
                (1, "2026", true),
                (0, "Zebra", true),
            ]
        );
        let closed: HashSet<String> = ["Receipts".to_string()].into();
        assert!(!rows(&folders, &closed).iter().any(|r| r.label == "2026"));
        assert!(rows(&folders, &closed).iter().find(|r| r.label == "Receipts").is_some_and(|r| r.has_children));
    }
}
