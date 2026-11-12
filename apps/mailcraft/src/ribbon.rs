//! The ribbon: Outlook's layout of tabs and groups (File, Home, Send / Receive, Folder, View,
//! Help), with large and small buttons. Every button, menu item and shortcut is an `Action`;
//! the app decides what each does and when it is available. Icons are drawn here in code.

use crate::foldertree::{self, Kind};
use egui::{Color32, Painter, Pos2, Rect, Sense, Stroke, pos2, vec2};
use serde::{Deserialize, Serialize};
use wordcraft_ui_egui::theme::{Tokens, regular, semibold};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tab {
    File,
    Home,
    SendReceive,
    Folder,
    View,
    Help,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::File, Tab::Home, Tab::SendReceive, Tab::Folder, Tab::View, Tab::Help];
    fn label(self) -> &'static str {
        match self {
            Tab::File => "File",
            Tab::Home => "Home",
            Tab::SendReceive => "Send / Receive",
            Tab::Folder => "Folder",
            Tab::View => "View",
            Tab::Help => "Help",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Unread,
    Flagged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneAt {
    Right,
    Bottom,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortKey {
    Date,
    From,
    Subject,
    Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // File
    AddAccount,
    RemoveAccount,
    SaveAs,
    Print,
    ViewSource,
    Exit,
    // Home
    NewMail,
    Delete,
    /// Put messages in Trash back where they were deleted from.
    Restore,
    /// Put the mail last deleted back where it was (Ctrl+Z).
    Undo,
    Archive,
    Junk,
    NotJunk,
    Reply,
    ReplyAll,
    Forward,
    ForwardAttachment,
    EditDraft,
    MoveTo,
    CopyTo,
    Rules,
    CreateRule,
    RunRules,
    Categorise,
    ToggleFavourite,
    /// Colour the folder (None: no colour).
    FolderColour(Option<[u8; 3]>),
    ToggleRead,
    ToggleFlag,
    FocusSearch,
    Filter(Filter),
    // Send / Receive
    SendReceive,
    SendReceiveAll,
    /// Bring just the folder showing up to date (F5).
    RefreshFolder,
    /// The folder shown before, or after (as a browser goes between pages).
    GoBack,
    GoForward,
    // Folder
    NewFolder,
    NewSubfolder,
    RenameFolder,
    DeleteFolder,
    MarkAllRead,
    EmptyFolder,
    // View
    Pane(PaneAt),
    ToggleFolderPane,
    ToggleRibbon,
    SimplifiedRibbon,
    Conversations,
    Settings,
    /// Moving MailCraft to another PC (transfer.rs).
    ExportSettings,
    ImportSettings,
    BlockSender,
    /// Focused Inbox: this sender's mail always in Focused, or always in Other.
    AlwaysFocused,
    AlwaysOther,
    /// Run the quick step at this place in the account's list.
    QuickStep(u8),
    ManageQuickSteps,
    Sweep,
    Theme(crate::looks::Theme),
    DarkMessages,
    Sort(SortKey),
    SortDirection,
    // Help
    Shortcuts,
    About,
    /// Open the folder MailCraft's log is in.
    OpenLogs,
}

/// How a button looks in the app's current state.
#[derive(Clone, Copy, Default)]
pub struct State {
    pub enabled: bool,
    pub checked: bool,
}

#[derive(Clone, Copy)]
enum Icon {
    Kind(Kind),
    Account,
    AccountRemove,
    Save,
    Print,
    Source,
    Exit,
    NewMail,
    Reply,
    ReplyAll,
    Forward,
    Attach,
    Draft,
    Move,
    Copy,
    Unread,
    Flag,
    Search,
    Funnel,
    Sync,
    SyncAll,
    FolderPlus,
    Rename,
    FolderMinus,
    ReadAll,
    Empty,
    PaneRight,
    PaneBottom,
    PaneOff,
    FolderPane,
    Sort,
    Keyboard,
    About,
    LogFolder,
    NotJunk,
    Restore,
    GoBack,
    GoForward,
    Collapse,
    Rules,
    Tag,
    Star,
    Conversation,
    Gear,
    Block,
    Steps,
    Sweep,
    Shade,
    Contrast,
}

enum Item {
    Big(Action, &'static str, Icon),
    /// Small buttons stacked in a column (up to three).
    Col(Vec<(Action, &'static str, Icon)>),
    /// Buttons named at run time (Quick steps), in columns of three.
    Steps(Vec<(Action, String)>),
}

struct Group {
    name: &'static str,
    items: Vec<Item>,
}

fn groups(tab: Tab, quick: &[String]) -> Vec<Group> {
    use crate::looks::Theme as T;
    use Action::*;
    use Item::{Big, Col};
    let g = |name, items| Group { name, items };
    match tab {
        Tab::File => vec![
            g("Accounts", vec![Big(AddAccount, "Add\naccount", Icon::Account), Big(RemoveAccount, "Remove\naccount", Icon::AccountRemove)]),
            g("Message", vec![Big(SaveAs, "Save as", Icon::Save), Big(Print, "Print", Icon::Print), Big(ViewSource, "View\nsource", Icon::Source)]),
            g("Options", vec![Big(Settings, "Options", Icon::Gear)]),
            g("Move to another PC", vec![Big(ExportSettings, "Export\nsettings", Icon::Save), Big(ImportSettings, "Import\nsettings", Icon::Move)]),
            g("Close", vec![Big(Exit, "Exit", Icon::Exit)]),
        ],
        Tab::Home => vec![
            g("New", vec![Big(NewMail, "New\nemail", Icon::NewMail)]),
            g(
                "Delete",
                vec![
                    Big(Delete, "Delete", Icon::Kind(Kind::Trash)),
                    Big(Archive, "Archive", Icon::Kind(Kind::Archive)),
                    Col(vec![
                        (Junk, "Junk", Icon::Kind(Kind::Junk)),
                        (NotJunk, "Not junk", Icon::NotJunk),
                        (BlockSender, "Block sender", Icon::Block),
                    ]),
                    Col(vec![(Sweep, "Sweep", Icon::Sweep), (Restore, "Restore", Icon::Restore)]),
                ],
            ),
            g(
                "Respond",
                vec![
                    Big(Reply, "Reply", Icon::Reply),
                    Big(ReplyAll, "Reply\nall", Icon::ReplyAll),
                    Big(Forward, "Forward", Icon::Forward),
                    Col(vec![(ForwardAttachment, "Forward as attachment", Icon::Attach), (EditDraft, "Edit draft", Icon::Draft)]),
                ],
            ),
            g(
                "Move",
                vec![
                    Big(MoveTo, "Move", Icon::Move),
                    Col(vec![(CopyTo, "Copy to folder", Icon::Copy), (Rules, "Rules", Icon::Rules), (CreateRule, "Create rule", Icon::Rules)]),
                ],
            ),
            g(
                "Quick steps",
                if quick.is_empty() {
                    vec![Col(vec![(ManageQuickSteps, "Create new", Icon::Steps)])]
                } else {
                    vec![Item::Steps(quick.iter().enumerate().map(|(i, n)| (QuickStep(i.min(255) as u8), n.clone())).collect())]
                },
            ),
            g(
                "Tags",
                vec![Big(ToggleRead, "Unread/\nRead", Icon::Unread), Big(Categorise, "Categorise", Icon::Tag), Big(ToggleFlag, "Flag", Icon::Flag)],
            ),
            g(
                "Find",
                vec![
                    Big(FocusSearch, "Search", Icon::Search),
                    Col(vec![
                        (Filter(self::Filter::All), "All mail", Icon::Funnel),
                        (Filter(self::Filter::Unread), "Unread", Icon::Unread),
                        (Filter(self::Filter::Flagged), "Flagged", Icon::Flag),
                    ]),
                ],
            ),
            g("Send/Receive", vec![Big(SendReceive, "Send/\nReceive", Icon::Sync)]),
        ],
        Tab::SendReceive => {
            vec![g("Send & Receive", vec![Big(SendReceive, "This\naccount", Icon::Sync), Big(SendReceiveAll, "All\naccounts", Icon::SyncAll)])]
        }
        Tab::Folder => vec![
            g("New", vec![Big(NewFolder, "New\nfolder", Icon::FolderPlus), Col(vec![(NewSubfolder, "New subfolder", Icon::FolderPlus)])]),
            g("Actions", vec![Big(RenameFolder, "Rename\nfolder", Icon::Rename), Big(DeleteFolder, "Delete\nfolder", Icon::FolderMinus)]),
            g("Clean Up", vec![Big(MarkAllRead, "Mark all\nas read", Icon::ReadAll), Big(EmptyFolder, "Empty\nfolder", Icon::Empty)]),
            g("Favourites", vec![Big(ToggleFavourite, "Show in\nFavourites", Icon::Star)]),
            g("Rules", vec![Big(RunRules, "Run rules\nnow", Icon::Rules), Big(Rules, "Manage\nrules", Icon::Rules)]),
            g(
                "Refresh",
                vec![
                    Big(SendReceive, "Update\nfolders", Icon::Sync),
                    Col(vec![
                        (RefreshFolder, "Refresh this folder", Icon::Sync),
                        (GoBack, "Back", Icon::GoBack),
                        (GoForward, "Forward", Icon::GoForward),
                    ]),
                ],
            ),
        ],
        Tab::View => vec![
            g(
                "Layout",
                vec![
                    Big(Pane(PaneAt::Right), "Reading pane\nright", Icon::PaneRight),
                    Big(Pane(PaneAt::Bottom), "Reading pane\nbottom", Icon::PaneBottom),
                    Big(Pane(PaneAt::Off), "Reading pane\noff", Icon::PaneOff),
                    Big(ToggleFolderPane, "Folder\npane", Icon::FolderPane),
                    Col(vec![(SimplifiedRibbon, "Simplified ribbon", Icon::Collapse), (ToggleRibbon, "Show tabs only", Icon::Collapse)]),
                ],
            ),
            g("Messages", vec![Big(Conversations, "Show as\nconversations", Icon::Conversation)]),
            g(
                "Theme",
                vec![
                    Col(vec![
                        (Theme(T::System), "Use system setting", Icon::Shade),
                        (Theme(T::Light), "Light", Icon::Shade),
                        (Theme(T::Dark), "Dark", Icon::Shade),
                    ]),
                    Col(vec![
                        (Theme(T::ContrastBlack), "High contrast black", Icon::Contrast),
                        (Theme(T::ContrastWhite), "High contrast white", Icon::Contrast),
                        (DarkMessages, "Dark message background", Icon::Shade),
                    ]),
                ],
            ),
            g(
                "Arrangement",
                vec![
                    Col(vec![
                        (Sort(SortKey::Date), "Date", Icon::Sort),
                        (Sort(SortKey::From), "From", Icon::Sort),
                        (Sort(SortKey::Subject), "Subject", Icon::Sort),
                    ]),
                    Col(vec![(Sort(SortKey::Size), "Size", Icon::Sort), (SortDirection, "Reverse order", Icon::Sort)]),
                ],
            ),
        ],
        Tab::Help => vec![g(
            "Help",
            vec![
                Big(Shortcuts, "Keyboard\nshortcuts", Icon::Keyboard),
                Big(About, "About\nMailCraft", Icon::About),
                Big(OpenLogs, "Log\nfolder", Icon::LogFolder),
            ],
        )],
    }
}

const STRIP: f32 = 30.0;
const BAND: f32 = 100.0;
const ROW: f32 = 40.0;

/// Outlook's ribbon layouts: classic (large buttons in named groups) or simplified (one slim
/// row, the arrow at the bottom right switches between them), and "tabs only" (`collapsed`),
/// where clicking a tab shows the ribbon until a command is picked (`peek`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Fold {
    pub simplified: bool,
    pub collapsed: bool,
    pub peek: bool,
}

impl Fold {
    pub fn showing(&self) -> bool {
        !self.collapsed || self.peek
    }

    pub fn toggle(&mut self) {
        self.collapsed = !self.collapsed;
        self.peek = false;
    }
}

/// The ribbon's height as it is now.
/// Key tips, as Office shows them when Alt is pressed: a letter over each tab, then over each
/// command of the open tab. Typing the letters picks it.
#[derive(Debug, Clone, Default)]
pub struct KeyTips {
    /// 0 off, 1 over the tabs, 2 over the commands.
    pub level: u8,
    /// Letters typed so far (some codes have two).
    pub typed: String,
    /// What each code does, as drawn this frame.
    pub codes: Vec<(String, KeyTarget)>,
    spots: Vec<(String, Pos2)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTarget {
    Tab(Tab),
    Act(Action),
}

impl KeyTips {
    /// A code for something labelled so: its initials first, then its other letters, then a
    /// letter and a number.
    fn add(&mut self, label: &str, target: KeyTarget, at: Pos2) {
        let up = label.to_uppercase();
        let initials: Vec<char> = up.split(|c: char| !c.is_alphanumeric()).filter_map(|w| w.chars().next()).collect();
        let letters: Vec<char> = up.chars().filter(|c| c.is_alphanumeric()).collect();
        let used = |c: &str| self.codes.iter().any(|(k, _)| k == c);
        let mut code = initials.iter().chain(letters.iter()).map(|c| c.to_string()).find(|c| !used(c));
        if code.is_none() {
            let first = initials.first().copied().unwrap_or('X');
            code = (1..=9).map(|d| format!("{first}{d}")).find(|c| !used(c));
        }
        if let Some(code) = code {
            self.codes.push((code.clone(), target));
            self.spots.push((code, at));
        }
    }

    fn draw(&self, ui: &egui::Ui, t: &Tokens) {
        for (code, at) in &self.spots {
            let g = ui.painter().layout_no_wrap(code.clone(), semibold(11.0), t.ribbon);
            let r = Rect::from_center_size(*at, g.size() + vec2(8.0, 4.0));
            ui.painter().rect_filled(r, 3.0, t.text);
            ui.painter().galley(r.center() - g.size() / 2.0, g, t.ribbon);
        }
    }
}

pub fn height(fold: &Fold) -> f32 {
    match (fold.showing(), fold.simplified) {
        (false, _) => STRIP + 4.0,
        (true, true) => STRIP + ROW,
        (true, false) => STRIP + BAND,
    }
}

/// Draw the tab strip and the open tab's groups. Returns the action clicked, if any.
pub fn show(
    ui: &mut egui::Ui,
    tab: &mut Tab,
    fold: &mut Fold,
    t: &Tokens,
    quick: &[String],
    state: &dyn Fn(Action) -> State,
    keys: &mut KeyTips,
) -> Option<Action> {
    keys.codes.clear();
    keys.spots.clear();
    let out = show_inner(ui, tab, fold, t, quick, state, keys);
    keys.draw(ui, t);
    out
}

fn show_inner(
    ui: &mut egui::Ui,
    tab: &mut Tab,
    fold: &mut Fold,
    t: &Tokens,
    quick: &[String],
    state: &dyn Fn(Action) -> State,
    keys: &mut KeyTips,
) -> Option<Action> {
    let mut out = None;
    // Tab strip.
    let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), STRIP), Sense::hover());
    let mut x = strip.left() + 10.0;
    for tb in Tab::ALL {
        let galley = ui.painter().layout_no_wrap(tb.label().to_string(), regular(13.0), t.text);
        let r = Rect::from_min_size(pos2(x, strip.top()), vec2(galley.size().x + 22.0, 30.0));
        let resp = ui.interact(r, ui.id().with(("tab", tb.label())), Sense::click());
        let on = *tab == tb;
        if resp.hovered() && !on {
            ui.painter().rect_filled(r.shrink2(vec2(2.0, 4.0)), 3.0, t.hover);
        }
        let colour = if on { t.accent } else { t.text };
        let font = if on { semibold(13.0) } else { regular(13.0) };
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, tb.label(), font, colour);
        if on {
            ui.painter().line_segment([pos2(r.left() + 8.0, r.bottom() - 2.0), pos2(r.right() - 8.0, r.bottom() - 2.0)], Stroke::new(2.5, t.accent));
        }
        if keys.level == 1 {
            keys.add(tb.label(), KeyTarget::Tab(tb), r.center_bottom());
        }
        if resp.double_clicked() {
            // As in Office: double-click a tab to collapse or pin the ribbon.
            fold.toggle();
        } else if resp.clicked() {
            if fold.collapsed {
                fold.peek = !(fold.peek && *tab == tb);
            }
            *tab = tb;
        }
        x = r.right() + 2.0;
    }
    if !fold.showing() {
        ui.painter().line_segment([pos2(strip.left(), strip.bottom() + 3.0), pos2(strip.right(), strip.bottom() + 3.0)], Stroke::new(1.0, t.border));
        return None;
    }
    if fold.simplified {
        let out = simplified(ui, *tab, fold, t, quick, state, keys);
        if out.is_some() {
            fold.peek = false;
        }
        return out;
    }
    // Groups.
    let (band, _) = ui.allocate_exact_size(vec2(ui.available_width(), BAND), Sense::hover());
    ui.painter().line_segment([band.left_top(), band.right_top()], Stroke::new(1.0, t.border));
    let mut x = band.left() + 8.0;
    // Groups that do not fit go into a "more" menu at the end, as Outlook folds its ribbon.
    let room = band.right() - 44.0;
    let mut spill: Vec<(Action, String)> = vec![];
    for g in groups(*tab, quick) {
        if !spill.is_empty() || x + group_width(ui, &g) > room {
            for it in &g.items {
                match it {
                    Item::Big(a, l, _) => spill.push((*a, l.replace("/\n", "/").replace('\n', " "))),
                    Item::Col(rows) => spill.extend(rows.iter().map(|(a, l, _)| (*a, l.to_string()))),
                    Item::Steps(steps) => spill.extend(steps.iter().cloned()),
                }
            }
            continue;
        }
        let start = x;
        for item in &g.items {
            match item {
                Item::Big(a, label, icon) => {
                    let r = Rect::from_min_size(pos2(x, band.top() + 5.0), vec2(big_width(ui, label), 66.0));
                    if button(ui, r, *a, label, *icon, true, t, state(*a)) {
                        out = Some(*a);
                    }
                    if keys.level == 2 && state(*a).enabled {
                        keys.add(label, KeyTarget::Act(*a), r.center_bottom() - vec2(0.0, 6.0));
                    }
                    x = r.right() + 2.0;
                }
                Item::Steps(steps) => {
                    for chunk in steps.chunks(3) {
                        let w = chunk.iter().map(|(_, l)| text_width(ui, l, 12.0)).fold(0.0_f32, f32::max) + 30.0;
                        for (i, (a, label)) in chunk.iter().enumerate() {
                            let r = Rect::from_min_size(pos2(x, band.top() + 5.0 + i as f32 * 22.0), vec2(w, 21.0));
                            if button(ui, r, *a, label, Icon::Steps, false, t, state(*a)) {
                                out = Some(*a);
                            }
                            if keys.level == 2 && state(*a).enabled {
                                keys.add(label, KeyTarget::Act(*a), r.left_center() + vec2(8.0, 4.0));
                            }
                        }
                        x += w + 4.0;
                    }
                }
                Item::Col(rows) => {
                    let w = rows.iter().map(|(_, l, _)| text_width(ui, l, 12.0)).fold(0.0_f32, f32::max) + 30.0;
                    for (i, (a, label, icon)) in rows.iter().enumerate() {
                        let r = Rect::from_min_size(pos2(x, band.top() + 5.0 + i as f32 * 22.0), vec2(w, 21.0));
                        if button(ui, r, *a, label, *icon, false, t, state(*a)) {
                            out = Some(*a);
                        }
                        if keys.level == 2 && state(*a).enabled {
                            keys.add(label, KeyTarget::Act(*a), r.left_center() + vec2(8.0, 4.0));
                        }
                    }
                    x += w + 4.0;
                }
            }
        }
        let end = x.max(start + text_width(ui, g.name, 11.0) + 8.0);
        ui.painter().text(pos2((start + end) / 2.0, band.bottom() - 14.0), egui::Align2::CENTER_CENTER, g.name, regular(11.0), t.text_dim);
        ui.painter().line_segment([pos2(end + 4.0, band.top() + 8.0), pos2(end + 4.0, band.bottom() - 8.0)], Stroke::new(1.0, t.border));
        x = end + 10.0;
    }
    if !spill.is_empty() {
        let m = Rect::from_min_size(pos2(x, band.top() + 5.0), vec2(34.0, 66.0));
        let resp = ui.interact(m, ui.id().with("ribbon_spill"), Sense::click()).on_hover_text("More commands");
        if resp.hovered() {
            ui.painter().rect_filled(m, 3.0, t.hover);
        }
        for dx in [-6.0, 0.0, 6.0] {
            ui.painter().circle_filled(m.center() + vec2(dx, -6.0), 1.8, t.text);
        }
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(200.0);
            for (a, label) in &spill {
                let item = egui::Button::new(label.as_str()).frame_when_inactive(false).min_size(vec2(200.0, 22.0));
                if ui.add_enabled(state(*a).enabled, item).clicked() {
                    out = Some(*a);
                    ui.close();
                }
            }
        });
    }
    // The border under the ribbon, and Outlook's collapse (^) or, while peeking, pin button.
    ui.painter().line_segment([pos2(band.left(), band.bottom() - 0.5), pos2(band.right(), band.bottom() - 0.5)], Stroke::new(1.0, t.border));
    let b = Rect::from_min_size(pos2(band.right() - 30.0, band.bottom() - 24.0), vec2(22.0, 18.0));
    let resp = ui.interact(b, ui.id().with("ribbon_fold"), Sense::click());
    let tip = if fold.peek { "Keep the ribbon open (Ctrl+F1)" } else { "Simplified ribbon" };
    let resp = resp.on_hover_text(tip);
    if resp.hovered() {
        ui.painter().rect_filled(b, 3.0, t.hover);
    }
    let c = b.center();
    let pen = Stroke::new(1.4, t.text_dim);
    if fold.peek {
        // A pin.
        ui.painter().circle_stroke(c + vec2(0.0, -3.0), 3.0, pen);
        ui.painter().line_segment([c + vec2(0.0, 0.0), c + vec2(0.0, 6.0)], pen);
        ui.painter().line_segment([c + vec2(-4.0, 0.0), c + vec2(4.0, 0.0)], pen);
    } else {
        ui.painter().line_segment([c + vec2(-5.0, 2.5), c + vec2(0.0, -2.5)], pen);
        ui.painter().line_segment([c + vec2(0.0, -2.5), c + vec2(5.0, 2.5)], pen);
    }
    if resp.clicked() {
        if fold.peek {
            fold.collapsed = false;
            fold.peek = false;
        } else {
            fold.simplified = true;
        }
    }
    if out.is_some() {
        fold.peek = false;
    }
    out
}

/// The simplified ribbon: the tab's commands as one row of small buttons, group by group;
/// what does not fit goes in a "more" menu at the end, as in Outlook.
fn simplified(
    ui: &mut egui::Ui,
    tab: Tab,
    fold: &mut Fold,
    t: &Tokens,
    quick: &[String],
    state: &dyn Fn(Action) -> State,
    keys: &mut KeyTips,
) -> Option<Action> {
    let mut out = None;
    let (band, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    ui.painter().line_segment([band.left_top(), band.right_top()], Stroke::new(1.0, t.border));
    ui.painter().line_segment([pos2(band.left(), band.bottom() - 0.5), pos2(band.right(), band.bottom() - 0.5)], Stroke::new(1.0, t.border));
    // Room kept at the right for "more" and the expand arrow.
    let limit = band.right() - 76.0;
    let mut x = band.left() + 8.0;
    let mut overflow: Vec<(Action, String)> = vec![];
    for (gi, g) in groups(tab, quick).into_iter().enumerate() {
        let flat: Vec<(Action, &str, Icon)> = g
            .items
            .iter()
            .flat_map(|it| match it {
                Item::Big(a, l, i) => vec![(*a, *l, *i)],
                Item::Col(rows) => rows.clone(),
                Item::Steps(steps) => steps.iter().map(|(a, l)| (*a, l.as_str(), Icon::Steps)).collect(),
            })
            .collect();
        if gi > 0 && overflow.is_empty() {
            ui.painter().line_segment([pos2(x, band.top() + 9.0), pos2(x, band.bottom() - 9.0)], Stroke::new(1.0, t.border));
            x += 6.0;
        }
        for (a, label, icon) in flat {
            let label = label.replace("/\n", "/").replace('\n', " ");
            let w = text_width(ui, &label, 12.0) + 32.0;
            if !overflow.is_empty() || x + w > limit {
                overflow.push((a, label));
                continue;
            }
            let r = Rect::from_min_size(pos2(x, band.top() + 7.0), vec2(w, 26.0));
            if button(ui, r, a, &label, icon, false, t, state(a)) {
                out = Some(a);
            }
            if keys.level == 2 && state(a).enabled {
                keys.add(&label, KeyTarget::Act(a), r.center_bottom());
            }
            x = r.right() + 2.0;
        }
    }
    // "More" (the rest of this tab's commands) and the arrow back to the classic ribbon.
    if !overflow.is_empty() {
        let m = Rect::from_min_size(pos2(band.right() - 66.0, band.top() + 7.0), vec2(30.0, 26.0));
        let resp = ui.interact(m, ui.id().with("ribbon_more"), Sense::click()).on_hover_text("More commands");
        if resp.hovered() {
            ui.painter().rect_filled(m, 3.0, t.hover);
        }
        for dx in [-6.0, 0.0, 6.0] {
            ui.painter().circle_filled(m.center() + vec2(dx, 0.0), 1.6, t.text);
        }
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(200.0);
            for (a, label) in &overflow {
                let item = egui::Button::new(label.as_str()).frame_when_inactive(false).min_size(vec2(200.0, 22.0));
                if ui.add_enabled(state(*a).enabled, item).clicked() {
                    out = Some(*a);
                    ui.close();
                }
            }
        });
    }
    let b = Rect::from_min_size(pos2(band.right() - 30.0, band.top() + 11.0), vec2(22.0, 18.0));
    let resp = ui.interact(b, ui.id().with("ribbon_fold"), Sense::click());
    let resp = resp.on_hover_text(if fold.peek { "Keep the ribbon open (Ctrl+F1)" } else { "Classic ribbon" });
    if resp.hovered() {
        ui.painter().rect_filled(b, 3.0, t.hover);
    }
    let c = b.center();
    let pen = Stroke::new(1.4, t.text_dim);
    ui.painter().line_segment([c + vec2(-5.0, -2.5), c + vec2(0.0, 2.5)], pen);
    ui.painter().line_segment([c + vec2(0.0, 2.5), c + vec2(5.0, -2.5)], pen);
    if resp.clicked() {
        if fold.peek {
            fold.collapsed = false;
            fold.peek = false;
        } else {
            fold.simplified = false;
        }
    }
    out
}

fn text_width(ui: &egui::Ui, s: &str, size: f32) -> f32 {
    s.lines().map(|l| ui.painter().layout_no_wrap(l.to_string(), regular(size), Color32::BLACK).size().x).fold(0.0_f32, f32::max)
}

fn big_width(ui: &egui::Ui, label: &str) -> f32 {
    (text_width(ui, label, 12.0) + 14.0).max(48.0)
}

#[allow(clippy::too_many_arguments)]
fn button(ui: &mut egui::Ui, r: Rect, a: Action, label: &str, icon: Icon, big: bool, t: &Tokens, st: State) -> bool {
    let id = ui.id().with(("ribbon", format!("{a:?}")));
    let sense = if st.enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(r, id, sense);
    // Unavailable buttons fade, coloured icons too (a red flag must not look ready to use).
    let mut faded = ui.painter().clone();
    if !st.enabled {
        faded.multiply_opacity(0.45);
    }
    let p = &faded;
    if st.checked {
        p.rect_filled(r, 3.0, t.checked);
    } else if resp.hovered() && st.enabled {
        p.rect_filled(r, 3.0, t.hover);
    }
    let fg = t.text;
    let accent = t.accent;
    if big {
        draw(p, Rect::from_center_size(pos2(r.center().x, r.top() + 17.0), vec2(26.0, 26.0)), icon, fg, accent);
        for (i, line) in label.lines().enumerate() {
            p.text(pos2(r.center().x, r.top() + 36.0 + i as f32 * 14.0), egui::Align2::CENTER_TOP, line, regular(12.0), fg);
        }
    } else {
        draw(p, Rect::from_center_size(pos2(r.left() + 12.0, r.center().y), vec2(16.0, 16.0)), icon, fg, accent);
        p.text(pos2(r.left() + 26.0, r.center().y), egui::Align2::LEFT_CENTER, label, regular(12.0), fg);
    }
    st.enabled && resp.clicked()
}

/// Draw an icon into `r`, designed on a 16 x 16 grid and scaled to fit.
fn draw(p: &Painter, r: Rect, icon: Icon, c: Color32, accent: Color32) {
    if let Icon::Kind(k) = icon {
        // The folder pane's icons, drawn at their own 16 px in the middle of the box.
        foldertree::icon(p, Rect::from_center_size(r.center(), vec2(16.0, 16.0)), Some(k), c, accent);
        return;
    }
    let k = r.width() / 16.0;
    let at = |x: f32, y: f32| -> Pos2 { pos2(r.left() + x * k, r.top() + y * k) };
    let s = Stroke::new(1.2 * k.clamp(1.0, 1.4), c);
    let a = Stroke::new(1.3 * k.clamp(1.0, 1.4), accent);
    let line = |pts: &[(f32, f32)], st: Stroke| {
        p.add(egui::Shape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), st));
    };
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32, st: Stroke| {
        p.rect_stroke(Rect::from_min_max(at(x0, y0), at(x1, y1)), 1.0, st, egui::StrokeKind::Middle);
    };
    let envelope = |st: Stroke| {
        rect(1.5, 3.5, 14.5, 12.5, st);
        line(&[(1.5, 4.0), (8.0, 9.0), (14.5, 4.0)], st);
    };
    let arrow_left = |y: f32, x0: f32, x1: f32, st: Stroke| {
        // A curved reply arrow: from the right, bending down, head at the left.
        line(&[(x1, y + 5.0), (x1, y + 3.0), (x1 - 2.0, y), (x0, y)], st);
        line(&[(x0 + 3.0, y - 3.0), (x0, y), (x0 + 3.0, y + 3.0)], st);
    };
    let folder = |st: Stroke| {
        line(&[(1.5, 4.0), (6.0, 4.0), (7.5, 5.5), (14.5, 5.5), (14.5, 13.5), (1.5, 13.5), (1.5, 4.0)], st);
    };
    match icon {
        Icon::Kind(_) => {}
        Icon::Account => {
            p.circle_stroke(at(8.0, 5.0), 3.0 * k, s);
            line(&[(2.5, 14.5), (3.5, 11.0), (6.0, 9.5), (10.0, 9.5), (12.5, 11.0), (13.5, 14.5)], s);
            line(&[(12.0, 1.5), (12.0, 6.5)], a);
            line(&[(9.5, 4.0), (14.5, 4.0)], a);
        }
        Icon::AccountRemove => {
            p.circle_stroke(at(8.0, 5.0), 3.0 * k, s);
            line(&[(2.5, 14.5), (3.5, 11.0), (6.0, 9.5), (10.0, 9.5), (12.5, 11.0), (13.5, 14.5)], s);
            line(&[(9.5, 4.0), (14.5, 4.0)], Stroke::new(s.width, Color32::from_rgb(0xD1, 0x3B, 0x3B)));
        }
        Icon::Save => {
            rect(2.0, 2.0, 14.0, 14.0, s);
            rect(4.5, 2.0, 11.5, 6.5, s);
            rect(4.5, 9.0, 11.5, 14.0, a);
        }
        Icon::Print => {
            rect(4.0, 1.5, 12.0, 5.5, s);
            rect(1.5, 5.5, 14.5, 11.5, s);
            rect(4.0, 9.5, 12.0, 14.5, a);
        }
        Icon::Source => {
            line(&[(5.5, 3.5), (1.5, 8.0), (5.5, 12.5)], s);
            line(&[(10.5, 3.5), (14.5, 8.0), (10.5, 12.5)], s);
            line(&[(9.0, 2.5), (7.0, 13.5)], a);
        }
        Icon::Exit => {
            line(&[(9.0, 2.0), (2.5, 2.0), (2.5, 14.0), (9.0, 14.0)], s);
            line(&[(6.0, 8.0), (14.5, 8.0)], a);
            line(&[(11.5, 5.0), (14.5, 8.0), (11.5, 11.0)], a);
        }
        Icon::NewMail => {
            envelope(s);
            p.circle_filled(at(13.0, 12.5), 3.4 * k, accent);
            line(&[(11.3, 12.5), (14.7, 12.5)], Stroke::new(1.3, Color32::WHITE));
            line(&[(13.0, 10.8), (13.0, 14.2)], Stroke::new(1.3, Color32::WHITE));
        }
        Icon::Reply => arrow_left(6.0, 2.0, 14.0, a),
        Icon::ReplyAll => {
            arrow_left(6.0, 5.0, 14.0, a);
            line(&[(4.0, 3.0), (1.0, 6.0), (4.0, 9.0)], a);
        }
        Icon::Forward => {
            line(&[(2.0, 11.0), (2.0, 9.0), (4.0, 6.0), (14.0, 6.0)], a);
            line(&[(11.0, 3.0), (14.0, 6.0), (11.0, 9.0)], a);
        }
        Icon::Attach => {
            line(&[(10.5, 4.5), (10.5, 11.0), (8.0, 13.5), (5.5, 11.0), (5.5, 3.5), (7.5, 1.5), (9.0, 3.0), (9.0, 10.0)], a);
        }
        Icon::Draft => {
            line(&[(3.0, 1.5), (10.0, 1.5), (13.0, 4.5), (13.0, 14.5), (3.0, 14.5), (3.0, 1.5)], s);
            line(&[(6.0, 12.0), (12.5, 5.5)], a);
        }
        Icon::Move => {
            folder(s);
            line(&[(5.0, 9.5), (11.0, 9.5)], a);
            line(&[(9.0, 7.5), (11.0, 9.5), (9.0, 11.5)], a);
        }
        Icon::Copy => {
            rect(1.5, 1.5, 10.5, 10.5, s);
            rect(5.5, 5.5, 14.5, 14.5, a);
        }
        Icon::Unread => {
            envelope(s);
            p.circle_filled(at(13.0, 4.0), 2.6 * k, accent);
        }
        Icon::Flag => {
            let red = Color32::from_rgb(0xD1, 0x3B, 0x3B);
            line(&[(3.5, 15.0), (3.5, 1.5)], s);
            p.add(egui::Shape::convex_polygon(vec![at(3.5, 2.0), at(13.5, 2.0), at(11.0, 5.0), at(13.5, 8.0), at(3.5, 8.0)], red, Stroke::NONE));
        }
        Icon::Search => {
            p.circle_stroke(at(6.5, 6.5), 4.5 * k, s);
            line(&[(10.0, 10.0), (14.5, 14.5)], a);
        }
        Icon::Funnel => {
            line(&[(1.5, 2.5), (14.5, 2.5), (9.5, 8.5), (9.5, 13.5), (6.5, 14.5), (6.5, 8.5), (1.5, 2.5)], s);
        }
        Icon::Sync | Icon::SyncAll => {
            line(&[(13.5, 6.0), (11.5, 3.0), (8.0, 2.0), (4.5, 3.0), (2.5, 6.5)], a);
            line(&[(13.5, 2.5), (13.5, 6.0), (10.0, 6.0)], a);
            line(&[(2.5, 10.0), (4.5, 13.0), (8.0, 14.0), (11.5, 13.0), (13.5, 9.5)], a);
            line(&[(2.5, 13.5), (2.5, 10.0), (6.0, 10.0)], a);
            if matches!(icon, Icon::SyncAll) {
                p.circle_filled(at(8.0, 8.0), 1.6 * k, c);
            }
        }
        Icon::LogFolder => {
            folder(s);
            for y in [8.0, 10.0, 12.0] {
                line(&[(5.0, y), (11.0, y)], a);
            }
        }
        Icon::FolderPlus => {
            folder(s);
            line(&[(8.0, 7.0), (8.0, 12.0)], a);
            line(&[(5.5, 9.5), (10.5, 9.5)], a);
        }
        Icon::Rename => {
            rect(1.5, 5.0, 14.5, 11.0, s);
            line(&[(4.0, 7.0), (4.0, 9.0)], a);
            line(&[(10.0, 2.0), (10.0, 14.0)], s);
            line(&[(8.5, 2.0), (11.5, 2.0)], s);
            line(&[(8.5, 14.0), (11.5, 14.0)], s);
        }
        Icon::FolderMinus => {
            folder(s);
            line(&[(5.5, 9.5), (10.5, 9.5)], Stroke::new(a.width, Color32::from_rgb(0xD1, 0x3B, 0x3B)));
        }
        Icon::ReadAll => {
            rect(1.5, 4.5, 12.5, 12.5, s);
            line(&[(1.5, 5.0), (7.0, 9.0), (12.5, 5.0)], s);
            line(&[(8.5, 13.0), (10.5, 15.0), (15.0, 10.0)], a);
        }
        Icon::Empty => {
            line(&[(2.0, 4.0), (14.0, 4.0)], s);
            line(&[(3.5, 4.0), (4.5, 14.5), (11.5, 14.5), (12.5, 4.0)], s);
            line(&[(6.0, 1.5), (10.0, 1.5)], s);
            line(&[(5.5, 7.5), (10.5, 12.0)], a);
            line(&[(10.5, 7.5), (5.5, 12.0)], a);
        }
        Icon::PaneRight | Icon::PaneBottom | Icon::PaneOff | Icon::FolderPane => {
            rect(1.5, 2.5, 14.5, 13.5, s);
            let fill = |x0: f32, y0: f32, x1: f32, y1: f32| {
                p.rect_filled(Rect::from_min_max(at(x0, y0), at(x1, y1)), 0.0, accent.linear_multiply(0.6));
            };
            match icon {
                Icon::PaneRight => fill(8.5, 3.5, 13.5, 12.5),
                Icon::PaneBottom => fill(2.5, 8.5, 13.5, 12.5),
                Icon::FolderPane => fill(2.5, 3.5, 5.5, 12.5),
                _ => {}
            }
            line(&[(4.0, 5.5), (7.0, 5.5)], s);
            line(&[(4.0, 7.5), (7.0, 7.5)], s);
        }
        Icon::Sort => {
            line(&[(4.5, 2.0), (4.5, 14.0)], s);
            line(&[(2.0, 11.5), (4.5, 14.0), (7.0, 11.5)], s);
            line(&[(9.0, 4.0), (14.5, 4.0)], a);
            line(&[(9.0, 8.0), (13.0, 8.0)], a);
            line(&[(9.0, 12.0), (11.5, 12.0)], a);
        }
        Icon::Keyboard => {
            rect(1.0, 4.0, 15.0, 12.5, s);
            for y in [6.5, 8.5] {
                for x in [3.5, 6.0, 8.5, 11.0, 13.0] {
                    p.circle_filled(at(x, y), 0.6 * k, c);
                }
            }
            line(&[(5.0, 10.8), (11.0, 10.8)], a);
        }
        Icon::About => {
            p.circle_stroke(at(8.0, 8.0), 6.5 * k, s);
            p.circle_filled(at(8.0, 4.8), 0.9 * k, accent);
            line(&[(8.0, 7.0), (8.0, 12.0)], a);
        }
        Icon::Gear => gear(p, r, c, accent),
        Icon::Steps => {
            // A lightning bolt.
            p.add(egui::Shape::convex_polygon(vec![at(9.5, 1.5), at(3.5, 9.0), at(7.5, 9.0)], Color32::from_rgb(0xE8, 0xA3, 0x17), Stroke::NONE));
            p.add(egui::Shape::convex_polygon(vec![at(8.5, 7.0), at(12.5, 7.0), at(6.5, 14.5)], Color32::from_rgb(0xE8, 0xA3, 0x17), Stroke::NONE));
        }
        Icon::Sweep => {
            line(&[(12.5, 1.5), (7.0, 8.5)], s);
            p.add(egui::Shape::convex_polygon(vec![at(5.0, 7.5), at(9.0, 10.5), at(7.0, 14.5), at(1.5, 12.0)], accent, Stroke::NONE));
        }
        Icon::Block => {
            let red = Color32::from_rgb(0xD1, 0x3B, 0x3B);
            p.circle_stroke(at(6.0, 5.0), 2.6 * k, s);
            line(&[(1.5, 13.5), (2.5, 10.0), (4.5, 8.8), (7.5, 8.8), (9.0, 9.5)], s);
            p.circle_stroke(at(12.0, 11.5), 3.2 * k, Stroke::new(1.3, red));
            line(&[(9.8, 13.7), (14.2, 9.3)], Stroke::new(1.3, red));
        }
        Icon::Conversation => {
            rect(1.5, 1.5, 11.0, 8.0, s);
            line(&[(3.5, 8.0), (3.0, 10.5), (6.0, 8.0)], s);
            rect(5.0, 6.0, 14.5, 12.5, a);
            line(&[(12.5, 12.5), (13.0, 15.0), (10.0, 12.5)], a);
        }
        Icon::Shade => {
            p.circle_stroke(at(8.0, 8.0), 6.0 * k, s);
            let half: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let ang = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (i as f32) / 12.0;
                    at(8.0 + 6.0 * ang.cos(), 8.0 + 6.0 * ang.sin())
                })
                .collect();
            p.add(egui::Shape::convex_polygon(half, c, Stroke::NONE));
        }
        Icon::Contrast => {
            p.rect_filled(Rect::from_min_max(at(1.5, 2.5), at(8.0, 13.5)), 0.0, Color32::BLACK);
            p.rect_filled(Rect::from_min_max(at(8.0, 2.5), at(14.5, 13.5)), 0.0, Color32::WHITE);
            rect(1.5, 2.5, 14.5, 13.5, s);
            p.text(at(4.8, 8.0), egui::Align2::CENTER_CENTER, "A", egui::FontId::proportional(8.0 * k), Color32::WHITE);
            p.text(at(11.3, 8.0), egui::Align2::CENTER_CENTER, "A", egui::FontId::proportional(8.0 * k), Color32::BLACK);
        }
        Icon::Tag => {
            line(&[(2.0, 2.0), (8.5, 2.0), (14.5, 8.0), (8.0, 14.5), (2.0, 8.5), (2.0, 2.0)], s);
            p.circle_filled(at(5.5, 5.5), 1.4 * k, accent);
            p.add(egui::Shape::convex_polygon(
                vec![at(9.0, 6.0), at(12.5, 9.0), at(9.5, 12.0), at(6.5, 9.5)],
                Color32::from_rgb(0xE7, 0x48, 0x56),
                Stroke::NONE,
            ));
        }
        Icon::Star => {
            let pts: Vec<Pos2> = (0..10)
                .map(|i| {
                    let r = if i % 2 == 0 { 7.0 } else { 3.0 };
                    let ang = std::f32::consts::PI * (i as f32) / 5.0 - std::f32::consts::FRAC_PI_2;
                    at(8.0 + r * ang.cos(), 8.5 + r * ang.sin())
                })
                .collect();
            p.add(egui::Shape::closed_line(pts, Stroke::new(1.3, Color32::from_rgb(0xE8, 0xA3, 0x17))));
        }
        Icon::Rules => {
            line(&[(1.5, 3.0), (8.0, 3.0)], s);
            line(&[(1.5, 7.0), (8.0, 7.0)], s);
            line(&[(1.5, 11.0), (6.0, 11.0)], s);
            line(&[(9.0, 7.0), (14.0, 7.0), (14.0, 12.5), (10.0, 12.5)], a);
            line(&[(12.0, 10.5), (10.0, 12.5), (12.0, 14.5)], a);
        }
        Icon::Collapse => {
            rect(1.5, 2.5, 14.5, 13.5, s);
            line(&[(1.5, 6.0), (14.5, 6.0)], s);
            line(&[(5.0, 11.0), (8.0, 8.5), (11.0, 11.0)], a);
        }
        Icon::NotJunk => {
            envelope(s);
            line(&[(9.5, 12.5), (11.5, 14.5), (15.0, 10.5)], Stroke::new(1.4, Color32::from_rgb(0x2E, 0x8B, 0x57)));
        }
        Icon::Restore => {
            // A bin, with an arrow up and out of it.
            line(&[(2.5, 7.5), (3.5, 14.5), (12.5, 14.5), (13.5, 7.5)], s);
            line(&[(8.0, 12.0), (8.0, 2.0)], a);
            line(&[(5.0, 5.0), (8.0, 2.0), (11.0, 5.0)], a);
        }
        Icon::GoBack => {
            line(&[(14.0, 8.0), (2.5, 8.0)], a);
            line(&[(7.0, 3.5), (2.5, 8.0), (7.0, 12.5)], a);
        }
        Icon::GoForward => {
            line(&[(2.0, 8.0), (13.5, 8.0)], a);
            line(&[(9.0, 3.5), (13.5, 8.0), (9.0, 12.5)], a);
        }
    }
}

/// A gear (settings) in `r`.
pub(crate) fn gear(p: &Painter, r: Rect, c: Color32, accent: Color32) {
    let k = r.width() / 16.0;
    let ctr = r.center();
    let teeth: Vec<Pos2> = (0..16)
        .map(|i| {
            let rad = if i % 2 == 0 { 7.0 } else { 5.6 } * k;
            let ang = std::f32::consts::TAU * (i as f32) / 16.0;
            ctr + vec2(rad * ang.cos(), rad * ang.sin())
        })
        .collect();
    p.add(egui::Shape::closed_line(teeth, Stroke::new(1.3, c)));
    p.circle_stroke(ctr, 2.4 * k, Stroke::new(1.3, accent));
}

/// How wide a group draws in the classic ribbon.
fn group_width(ui: &egui::Ui, g: &Group) -> f32 {
    let items: f32 = g
        .items
        .iter()
        .map(|it| match it {
            Item::Big(_, l, _) => big_width(ui, l) + 2.0,
            Item::Col(rows) => rows.iter().map(|(_, l, _)| text_width(ui, l, 12.0)).fold(0.0_f32, f32::max) + 34.0,
            Item::Steps(steps) => steps.chunks(3).map(|c| c.iter().map(|(_, l)| text_width(ui, l, 12.0)).fold(0.0_f32, f32::max) + 34.0).sum(),
        })
        .sum();
    items.max(text_width(ui, g.name, 11.0) + 8.0) + 10.0
}
