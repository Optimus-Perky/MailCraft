//! Upstream #2258: `!important` in any case and spacing.

use crate::html::parse;
use crate::model::{FBlock, Inline};

#[test]
fn important_is_read_in_any_case_and_spacing() {
    let f = parse(
        "<p style='margin-left: 24pt !IMPORTANT; text-align: center ! important'><span style='color: #ff0000 !IMPORTANT; font-size: 16pt ! important; font-weight: 700 !Important'>Styled</span></p>",
    );
    let FBlock::Para(p) = &f.blocks[0] else { panic!("no paragraph") };
    assert_eq!(p.align, Some(wordcraft_doc::Align::Center));
    assert_eq!(p.indent_left, Some(24.0));
    let Inline::Text(_, fmt) = &p.inlines[0] else { panic!("no text") };
    assert_eq!(fmt.color, Some(wordcraft_doc::Rgb(0xFF, 0, 0)));
    assert_eq!(fmt.size, Some(16.0));
    assert!(fmt.bold);
}
