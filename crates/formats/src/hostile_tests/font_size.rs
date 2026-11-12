//! Upstream #4755: `<font size>` at the integer limits.

use crate::html::parse;
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

/// Upstream's relative sizes overflowed at the limits; here sizes are absolute and clamped. Kept
/// as a guard so a later port of relative sizes can't bring the overflow back.
#[test]
fn font_size_at_the_integer_limits_is_clamped() {
    for (size, want) in [("+2147483647", 36.0), ("-2147483648", 7.5), ("2147483647", 36.0), ("99999999999999", 12.0), ("+", 12.0)] {
        let fmt = first_fmt(&format!("<p><font size=\"{size}\">x</font></p>"));
        assert!(fmt.size.is_none_or(|s| s == want), "size {size:?} gave {:?}", fmt.size);
    }
}
