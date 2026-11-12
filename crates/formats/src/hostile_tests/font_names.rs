//! Upstream #3862: font names written into exported CSS.

use wordcraft_doc::props::CharProps;
use wordcraft_doc::{Document, Paragraph, para_block};

use crate::html::export;

/// A font name from a received message is written into the reply's CSS: a backslash or a control
/// character must not end the quoted name and let the rest of it run on as CSS.
#[test]
fn font_names_cannot_break_out_of_exported_css() {
    for name in ["Evil\\", "Evil\\'", "Evil\n:red", "Evil\u{0}x"] {
        let mut p = Paragraph::new();
        let _ = p.insert_text(0, "x", &CharProps { font: Some(name.into()), ..Default::default() });
        let mut d = Document::new();
        d.body = vec![para_block(p)];
        let html = export(&d);
        let css = html.rsplit("font-family:").next().unwrap_or("").split(['"', '>']).next().unwrap_or("").to_string();
        assert!(!css.contains('\\') && !css.chars().any(char::is_control), "{name:?} wrote {css:?}");
        assert!(css.starts_with("'Evil"), "{name:?} wrote {css:?}");
    }
}
