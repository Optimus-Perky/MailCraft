//! Back and forward between folders, as a browser goes between pages (Alt+Left / Alt+Right,
//! and the mouse's side buttons). Going back or forward adds nothing; picking a folder after
//! going back drops what was ahead. Folders that have gone since are stepped over.

/// A folder that was shown: which account, and its name there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    pub account: String,
    pub folder: String,
}

/// How many folders are remembered; the oldest go first.
pub const CAP: usize = 50;

#[derive(Debug, Default)]
pub struct History {
    visits: Vec<Visit>,
    /// The one showing (meaningless while `visits` is empty).
    at: usize,
}

impl History {
    /// The folder now showing: remembered, unless it is the one already there (called every
    /// frame, so going back, which lands on a remembered folder, adds nothing).
    pub fn visit(&mut self, v: Visit) {
        if self.visits.get(self.at) == Some(&v) {
            return;
        }
        if !self.visits.is_empty() {
            self.visits.truncate(self.at.saturating_add(1));
        }
        self.visits.push(v);
        let over = self.visits.len().saturating_sub(CAP);
        if over > 0 {
            self.visits.drain(..over);
        }
        self.at = self.visits.len().saturating_sub(1);
    }

    /// Whether this folder is the one showing (checked every frame, before making a Visit).
    pub fn is_at(&self, account: &str, folder: &str) -> bool {
        self.visits.get(self.at).is_some_and(|v| v.account == account && v.folder == folder)
    }

    /// The nearest earlier folder that still exists, now the one showing.
    pub fn back(&mut self, exists: impl Fn(&Visit) -> bool) -> Option<Visit> {
        let i = self.find_back(&exists)?;
        self.at = i;
        self.visits.get(i).cloned()
    }

    /// The nearest later folder that still exists, now the one showing.
    pub fn forward(&mut self, exists: impl Fn(&Visit) -> bool) -> Option<Visit> {
        let i = self.find_forward(&exists)?;
        self.at = i;
        self.visits.get(i).cloned()
    }

    pub fn can_back(&self, exists: impl Fn(&Visit) -> bool) -> bool {
        self.find_back(&exists).is_some()
    }

    pub fn can_forward(&self, exists: impl Fn(&Visit) -> bool) -> bool {
        self.find_forward(&exists).is_some()
    }

    fn find_back(&self, exists: &dyn Fn(&Visit) -> bool) -> Option<usize> {
        let end = self.at.min(self.visits.len());
        (0..end).rev().find(|&i| self.visits.get(i).is_some_and(exists))
    }

    fn find_forward(&self, exists: &dyn Fn(&Visit) -> bool) -> Option<usize> {
        (self.at.saturating_add(1)..self.visits.len()).find(|&i| self.visits.get(i).is_some_and(exists))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(f: &str) -> Visit {
        Visit { account: "a".into(), folder: f.into() }
    }

    fn all(_: &Visit) -> bool {
        true
    }

    #[test]
    fn back_and_forward_like_a_browser() {
        let mut h = History::default();
        assert!(!h.can_back(all) && !h.can_forward(all));
        assert_eq!(h.back(all), None, "nothing yet");
        for f in ["INBOX", "INBOX", "Bills", "Projects"] {
            h.visit(v(f));
        }
        assert_eq!(h.back(all), Some(v("Bills")), "the same folder twice is one visit");
        // Landing there is seen by the app as a visit: it adds nothing.
        h.visit(v("Bills"));
        assert_eq!(h.back(all), Some(v("INBOX")));
        assert_eq!(h.back(all), None, "the start");
        assert!(h.can_forward(all));
        assert_eq!(h.forward(all), Some(v("Bills")));
        assert_eq!(h.forward(all), Some(v("Projects")));
        assert_eq!(h.forward(all), None, "the end");
        // A new folder after going back drops what was ahead.
        h.back(all);
        h.back(all);
        h.visit(v("Archive"));
        assert!(!h.can_forward(all));
        assert_eq!(h.back(all), Some(v("INBOX")));
        assert_eq!(h.forward(all), Some(v("Archive")));
    }

    #[test]
    fn folders_that_have_gone_are_stepped_over() {
        let mut h = History::default();
        for f in ["INBOX", "Old", "Bills", "Old", "Projects"] {
            h.visit(v(f));
        }
        let here = |x: &Visit| x.folder != "Old";
        assert_eq!(h.back(here), Some(v("Bills")));
        assert_eq!(h.back(here), Some(v("INBOX")));
        assert_eq!(h.forward(here), Some(v("Bills")));
        assert_eq!(h.forward(here), Some(v("Projects")));
        let none = |_: &Visit| false;
        assert!(!h.can_back(none));
        assert_eq!(h.back(none), None);
        assert_eq!(h.forward(here), None);
        // Accounts are part of it: the same name elsewhere is another folder.
        h.visit(Visit { account: "b".into(), folder: "Projects".into() });
        assert_eq!(h.back(all), Some(v("Projects")));
    }

    #[test]
    fn only_the_last_fifty_are_kept() {
        let mut h = History::default();
        for i in 0..CAP + 25 {
            h.visit(v(&format!("F{i}")));
        }
        let mut n = 0;
        while h.back(all).is_some() {
            n += 1;
        }
        assert_eq!(n, CAP - 1);
        assert_eq!(h.forward(all), Some(v("F26")), "the oldest kept is F25, where it now stands");
        // Going back to the start and visiting again keeps within the cap too.
        h.back(all);
        h.visit(v("New"));
        assert_eq!(h.back(all), Some(v("F25")));
    }
}
