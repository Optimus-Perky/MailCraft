//! Upstream #72, #681, #1235: link targets that leave the message.

use wordcraft_doc::props::CharProps;
use wordcraft_doc::{Document, Paragraph, para_block};

use crate::html::{export, parse};
use crate::model::{FBlock, Fmt, Inline};

fn first_fmt(html: &str) -> Fmt {
    let f = parse(html);
    for b in &f.blocks {
        if let FBlock::Para(p) = b {
            for i in &p.inlines {
                if let Inline::Text(_, fmt) = i {
                    return fmt.clone();
                }
            }
        }
    }
    Fmt::default()
}

const BAD_HREFS: &[&str] = &[
    "file:///C:/Windows/System32/calc.exe",
    "ms-msdt:/id PCWDiagnostic",
    "\\\\attacker.example\\share\\x",
    "//attacker.example/share/x",
    "/\\attacker.example\\x",
    "vbscript:MsgBox(1)",
    "java\nscript:alert(1)",
    "data:text/html,<script>alert(1)</script>",
    "search-ms:query=x",
];

/// A received message's link to a file, a share or a protocol handler is not kept as a link.
#[test]
fn unsafe_link_targets_are_not_imported() {
    for bad in BAD_HREFS {
        let html = format!("<p><a href=\"{}\">click</a></p>", crate::html::esc(bad));
        assert_eq!(first_fmt(&html).link, None, "{bad:?}");
    }
    for ok in ["https://example.com/a?b=1&c=2", "http://example.com", "mailto:a@example.com", "#top", "page.html"] {
        let html = format!("<p><a href=\"{}\">click</a></p>", crate::html::esc(ok));
        assert_eq!(first_fmt(&html).link.as_deref(), Some(ok), "{ok:?}");
    }
}

/// A reply or forward quotes the message through `html::export`: an unsafe target goes out as
/// plain text, never as an `href`.
#[test]
fn unsafe_link_targets_are_not_exported() {
    for bad in BAD_HREFS {
        let mut p = Paragraph::new();
        let _ = p.insert_text(0, "click", &CharProps { link: Some((*bad).into()), ..Default::default() });
        let mut d = Document::new();
        d.body = vec![para_block(p)];
        let html = export(&d);
        assert!(!html.contains("<a "), "{bad:?} exported as a link: {html}");
        assert!(html.contains("click"));
    }
    let mut p = Paragraph::new();
    let _ = p.insert_text(0, "ok", &CharProps { link: Some("https://example.com/".into()), ..Default::default() });
    let mut d = Document::new();
    d.body = vec![para_block(p)];
    assert!(export(&d).contains("<a href=\"https://example.com/\">ok</a>"));
}
