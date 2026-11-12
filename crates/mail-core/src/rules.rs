//! Mail rules, as in Outlook: when a message arrives (or when the rules are run on a folder),
//! check it against each rule in turn and do what the first matching rules say.
//!
//! Conditions look at what the cache holds for every message - sender, recipients, subject,
//! size - so rules work without downloading bodies. An empty condition is not used; a rule
//! with no condition matches nothing (a rule that moved every message would be a trap).

use crate::store::MessageRow;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub name: String,
    pub enabled: bool,
    /// The account it belongs to (folder names are per account).
    pub account: String,
    /// All conditions must hold (true), or any one of them (false).
    pub match_all: bool,
    /// Words to look for; several separated by ';' or ',' and any of them matches.
    pub from: String,
    pub to: String,
    pub subject: String,
    pub min_size_kb: Option<u32>,
    pub move_to: Option<String>,
    pub copy_to: Option<String>,
    pub mark_read: bool,
    pub flag: bool,
    /// Category keyword to give it.
    pub categorise: Option<String>,
    /// Into Trash.
    pub delete: bool,
    /// Stop processing more rules for a message this rule matched.
    pub stop: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            name: String::new(),
            enabled: true,
            account: String::new(),
            match_all: true,
            from: String::new(),
            to: String::new(),
            subject: String::new(),
            min_size_kb: None,
            move_to: None,
            copy_to: None,
            mark_read: false,
            flag: false,
            categorise: None,
            delete: false,
            stop: false,
        }
    }
}

fn terms(s: &str) -> Vec<String> {
    s.split([';', ',']).map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect()
}

fn has_any(hay: &str, words: &[String]) -> bool {
    let hay = hay.to_lowercase();
    words.iter().any(|w| hay.contains(w.as_str()))
}

impl Rule {
    pub fn has_condition(&self) -> bool {
        !terms(&self.from).is_empty() || !terms(&self.to).is_empty() || !terms(&self.subject).is_empty() || self.min_size_kb.is_some()
    }

    pub fn has_action(&self) -> bool {
        self.move_to.is_some() || self.copy_to.is_some() || self.mark_read || self.flag || self.categorise.is_some() || self.delete
    }

    pub fn matches(&self, m: &MessageRow) -> bool {
        if !self.enabled || !self.has_condition() {
            return false;
        }
        let mut tests: Vec<bool> = vec![];
        let from = terms(&self.from);
        if !from.is_empty() {
            let sender = format!("{} {}", m.from_name.as_deref().unwrap_or(""), m.from_addr.as_deref().unwrap_or(""));
            tests.push(has_any(&sender, &from));
        }
        let to = terms(&self.to);
        if !to.is_empty() {
            tests.push(has_any(m.to.as_deref().unwrap_or(""), &to));
        }
        let subject = terms(&self.subject);
        if !subject.is_empty() {
            tests.push(has_any(m.subject.as_deref().unwrap_or(""), &subject));
        }
        if let Some(kb) = self.min_size_kb {
            tests.push(m.size.unwrap_or(0) as u64 >= kb as u64 * 1024);
        }
        if self.match_all { tests.iter().all(|t| *t) } else { tests.iter().any(|t| *t) }
    }

    /// One line describing the rule, for the rules list.
    pub fn summary(&self) -> String {
        let mut when = vec![];
        if !self.from.trim().is_empty() {
            when.push(format!("from contains \"{}\"", self.from.trim()));
        }
        if !self.to.trim().is_empty() {
            when.push(format!("to contains \"{}\"", self.to.trim()));
        }
        if !self.subject.trim().is_empty() {
            when.push(format!("subject contains \"{}\"", self.subject.trim()));
        }
        if let Some(kb) = self.min_size_kb {
            when.push(format!("size at least {kb} KB"));
        }
        let mut then = vec![];
        if self.mark_read {
            then.push("mark as read".to_string());
        }
        if self.flag {
            then.push("flag".to_string());
        }
        if let Some(c) = &self.categorise {
            then.push(format!("categorise {c}"));
        }
        if let Some(f) = &self.copy_to {
            then.push(format!("copy to {f}"));
        }
        if self.delete {
            then.push("delete".to_string());
        } else if let Some(f) = &self.move_to {
            then.push(format!("move to {f}"));
        }
        if self.stop {
            then.push("stop".to_string());
        }
        let join = if self.match_all { " and " } else { " or " };
        format!("When {} : {}", when.join(join), then.join(", "))
    }
}

/// What the rules want done to a folder's messages, gathered so each kind of change is one
/// request to the server.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub mark_read: Vec<u32>,
    pub flag: Vec<u32>,
    /// (category keyword, UIDs)
    pub categorise: Vec<(String, Vec<u32>)>,
    /// (folder, UIDs)
    pub copy: Vec<(String, Vec<u32>)>,
    pub moves: Vec<(String, Vec<u32>)>,
    pub delete: Vec<u32>,
    /// Messages at least one rule matched.
    pub matched: usize,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.mark_read.is_empty()
            && self.flag.is_empty()
            && self.categorise.is_empty()
            && self.copy.is_empty()
            && self.moves.is_empty()
            && self.delete.is_empty()
    }
}

fn push(list: &mut Vec<(String, Vec<u32>)>, folder: &str, uid: u32) {
    match list.iter_mut().find(|(f, _)| f == folder) {
        Some((_, v)) => v.push(uid),
        None => list.push((folder.to_string(), vec![uid])),
    }
}

/// Run `rules` (in order) over `messages` that are in `folder`.
pub fn plan(rules: &[Rule], messages: &[MessageRow], folder: &str) -> Plan {
    let mut p = Plan::default();
    for m in messages {
        let mut hit = false;
        // A message can leave the folder once: the first move or delete wins.
        let mut gone = false;
        for r in rules.iter().filter(|r| r.matches(m)) {
            hit = true;
            if r.mark_read && !m.seen() && !p.mark_read.contains(&m.uid) {
                p.mark_read.push(m.uid);
            }
            if r.flag && !p.flag.contains(&m.uid) {
                p.flag.push(m.uid);
            }
            if let Some(c) = r.categorise.as_deref() {
                push(&mut p.categorise, c, m.uid);
            }
            if let Some(f) = r.copy_to.as_deref().filter(|f| *f != folder) {
                push(&mut p.copy, f, m.uid);
            }
            if !gone {
                if r.delete {
                    p.delete.push(m.uid);
                    gone = true;
                } else if let Some(f) = r.move_to.as_deref().filter(|f| *f != folder) {
                    push(&mut p.moves, f, m.uid);
                    gone = true;
                }
            }
            if r.stop {
                break;
            }
        }
        if hit {
            p.matched += 1;
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(uid: u32, from: &str, subject: &str, size: u32, seen: bool) -> MessageRow {
        MessageRow {
            id: uid as i64,
            uid,
            flags: if seen { "\\Seen".into() } else { String::new() },
            date: None,
            from_name: Some(from.split('<').next().unwrap_or("").trim().into()),
            from_addr: from.split('<').nth(1).map(|a| a.trim_end_matches('>').into()),
            to: Some("me@example.com".into()),
            subject: Some(subject.into()),
            size: Some(size),
            message_id: None,
            in_reply_to: None,
            has_body: false,
            folder: "INBOX".into(),
            categories: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn matches_and_plans() {
        let receipts = Rule {
            name: "Receipts".into(),
            from: "amazon; ebay".into(),
            move_to: Some("Receipts".into()),
            mark_read: true,
            categorise: Some("Bills".into()),
            ..Default::default()
        };
        let big = Rule { name: "Big".into(), min_size_kb: Some(500), flag: true, stop: true, ..Default::default() };
        let news = Rule { name: "News".into(), subject: "newsletter".into(), delete: true, ..Default::default() };
        let msgs = vec![
            msg(1, "Amazon <orders@amazon.co.uk>", "Your order", 2_000, false),
            msg(2, "Mark <mark@example.com>", "Photos", 900_000, false),
            msg(3, "Shop <x@shop.example>", "Weekly newsletter", 3_000, true),
            msg(4, "eBay <ebay@ebay.co.uk>", "Big newsletter", 600_000, false),
            msg(5, "Friend <f@example.com>", "Hello", 1_000, false),
        ];
        let p = plan(&[receipts.clone(), big, news], &msgs, "INBOX");
        assert_eq!(p.mark_read, vec![1, 4]);
        assert_eq!(p.flag, vec![2, 4]);
        // 4 matched "Receipts" (move) and "Big" (stop): it moves, and "News" never sees it.
        assert_eq!(p.moves, vec![("Receipts".to_string(), vec![1, 4])]);
        assert_eq!(p.delete, vec![3]);
        assert_eq!(p.categorise, vec![("Bills".to_string(), vec![1, 4])]);
        assert_eq!(p.matched, 4);
        // Run on the Receipts folder itself: nothing to move.
        assert!(plan(&[receipts], &msgs, "Receipts").moves.is_empty());
    }

    #[test]
    fn any_all_and_safety() {
        let r = Rule { from: "amazon".into(), subject: "invoice".into(), match_all: true, flag: true, ..Default::default() };
        assert!(!r.matches(&msg(1, "Amazon <a@amazon.com>", "Your order", 1, false)));
        assert!(Rule { match_all: false, ..r.clone() }.matches(&msg(1, "Amazon <a@amazon.com>", "Your order", 1, false)));
        assert!(!Rule { enabled: false, ..r.clone() }.matches(&msg(1, "Amazon <a@amazon.com>", "Invoice", 1, false)));
        // No condition: matches nothing.
        let none = Rule { flag: true, ..Default::default() };
        assert!(!none.has_condition() && !none.matches(&msg(1, "a <a@b>", "x", 1, false)));
        assert!(r.summary().contains("from contains \"amazon\" and subject contains \"invoice\""));
    }
}
