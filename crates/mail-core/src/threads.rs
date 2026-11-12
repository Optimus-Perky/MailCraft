//! Grouping messages into conversations, as Outlook's conversation view does: replies join the
//! message they answer (In-Reply-To), and messages whose subjects match once "Re:"/"Fwd:" are
//! taken off join too, when at least one of them is a reply or forward.

use crate::store::MessageRow;
use std::collections::HashMap;

/// A subject without its reply/forward prefixes, lower case: the conversation's topic.
pub fn topic(subject: &str) -> String {
    let mut t = subject.trim();
    loop {
        let lower = t.to_ascii_lowercase();
        let Some(p) = ["re:", "fw:", "fwd:", "aw:", "sv:", "vs:", "antw:", "réf:"].iter().find(|p| lower.starts_with(**p)) else { break };
        t = t.get(p.len()..).unwrap_or("").trim_start();
    }
    t.to_lowercase()
}

fn is_reply(m: &MessageRow) -> bool {
    m.in_reply_to.as_deref().is_some_and(|r| !r.trim().is_empty())
        || topic(m.subject.as_deref().unwrap_or("")) != m.subject.as_deref().unwrap_or("").trim().to_lowercase()
}

fn norm_id(id: &str) -> String {
    id.trim().trim_matches(['<', '>']).to_ascii_lowercase()
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while let Some(&p) = parent.get(i) {
        if p == i {
            break;
        }
        let gp = parent.get(p).copied().unwrap_or(p);
        if let Some(slot) = parent.get_mut(i) {
            *slot = gp;
        }
        i = p;
    }
    i
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        // Keep the earlier one as the root, so a group keeps its first member's place.
        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
        if let Some(slot) = parent.get_mut(hi) {
            *slot = lo;
        }
    }
}

/// Conversations among `messages`, as lists of indices. Each list keeps the input order, and
/// the lists come in the order of their first member, so a list sorted newest first gives
/// conversations newest first, each with its newest message first.
pub fn group(messages: &[MessageRow]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..messages.len()).collect();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for (i, m) in messages.iter().enumerate() {
        if let Some(id) = m.message_id.as_deref().filter(|s| !s.trim().is_empty()) {
            by_id.entry(norm_id(id)).or_insert(i);
        }
    }
    for (i, m) in messages.iter().enumerate() {
        if let Some(&p) = m.in_reply_to.as_deref().and_then(|r| by_id.get(&norm_id(r))) {
            union(&mut parent, i, p);
        }
    }
    let mut by_topic: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, m) in messages.iter().enumerate() {
        let t = topic(m.subject.as_deref().unwrap_or(""));
        if !t.is_empty() {
            by_topic.entry(t).or_default().push(i);
        }
    }
    for members in by_topic.values() {
        if members.len() > 1
            && members.iter().any(|&i| messages.get(i).is_some_and(is_reply))
            && let Some(&first) = members.first()
        {
            for &i in members.iter().skip(1) {
                union(&mut parent, first, i);
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = vec![];
    let mut slot: HashMap<usize, usize> = HashMap::new();
    for i in 0..messages.len() {
        let root = find(&mut parent, i);
        match slot.get(&root) {
            Some(&g) => {
                if let Some(v) = groups.get_mut(g) {
                    v.push(i);
                }
            }
            None => {
                slot.insert(root, groups.len());
                groups.push(vec![i]);
            }
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(uid: u32, subject: &str, id: &str, reply_to: Option<&str>) -> MessageRow {
        MessageRow {
            id: uid as i64,
            uid,
            flags: String::new(),
            date: None,
            from_name: None,
            from_addr: None,
            to: None,
            subject: Some(subject.into()),
            size: None,
            message_id: Some(id.into()),
            in_reply_to: reply_to.map(str::to_string),
            has_body: false,
            folder: "INBOX".into(),
            categories: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn groups_replies_and_topics() {
        // Newest first, as the list shows them.
        let msgs = vec![
            m(5, "Re: Re: Strap fit", "<e@x>", Some("<c@x>")),
            m(4, "Invoice 1234", "<d@x>", None),
            m(3, "RE: Strap fit", "<c@x>", None), // a reply with no In-Reply-To (some clients)
            m(2, "Weekly news", "<b@x>", None),
            m(1, "Strap fit", "<a@x>", None),
            m(0, "Weekly news", "<z@x>", None), // same subject, but nobody replied: stays apart
        ];
        let g = group(&msgs);
        assert_eq!(g, vec![vec![0, 2, 4], vec![1], vec![3], vec![5]]);
        assert_eq!(topic("Fwd: RE:  Strap fit"), "strap fit");
    }

    #[test]
    fn in_reply_to_joins_across_subjects() {
        let msgs = vec![m(2, "Thanks!", "<b@x>", Some("<a@x>")), m(1, "Photos from Saturday", "<a@x>", None)];
        assert_eq!(group(&msgs), vec![vec![0, 1]]);
    }
}
