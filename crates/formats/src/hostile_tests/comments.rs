//! Upstream #5040: empty comments.

use crate::html::parse;
use crate::model::{FBlock, Flow};

fn paras(f: &Flow) -> Vec<String> {
    f.blocks
        .iter()
        .filter_map(|b| match b {
            FBlock::Para(p) => Some(p.text()),
            _ => None,
        })
        .collect()
}

/// `<!-->` and `<!--->` are whole (empty) comments; read as an opening one they hid the rest of
/// the message.
#[test]
fn empty_comments_do_not_swallow_the_message() {
    let f = parse("<p>one<!-->two</p><p>three<!--->four</p><p>five<!-- x -->six</p>");
    assert_eq!(paras(&f), vec!["onetwo".to_string(), "threefour".to_string(), "fivesix".to_string()]);
}
