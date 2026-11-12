//! Upstream #4820: character references written without `;` (and #3249: numeric ones are digits).

use crate::html::parse;
use crate::model::{FBlock, Flow, Fmt, Inline};

fn paras(f: &Flow) -> Vec<String> {
    f.blocks
        .iter()
        .filter_map(|b| match b {
            FBlock::Para(p) => Some(p.text()),
            _ => None,
        })
        .collect()
}

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

#[test]
fn references_without_semicolon_decode_as_browsers_do() {
    let f = parse("<p>&amp &lt;b&gt &copy 2026 &#169 &#x41 &notanentity x&ampy</p>");
    assert_eq!(paras(&f).concat(), "& <b> © 2026 © A &notanentity x&y");
    // Numeric references are digits only (upstream #3249); out of range is U+FFFD.
    let f = parse("<p>&#+65; &#x-41; &#99999999999; &#x110000;</p>");
    assert_eq!(paras(&f).concat(), "&#+65; &#x-41; \u{FFFD} \u{FFFD}");
    // In an attribute a legacy name followed by `=` or a letter stays text (query strings).
    assert_eq!(
        first_fmt("<p><a href=\"https://example.com/?a=1&copy=2&amp;lt=3\">x</a></p>").link.as_deref(),
        Some("https://example.com/?a=1&copy=2&lt=3")
    );
}
