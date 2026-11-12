//! Upstream #4725: a <title> outside <head>.

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

#[test]
fn title_outside_head_stays_out_of_the_body() {
    let f = parse("<html><body><title>Hidden title</title><p>Body</p></body></html>");
    assert_eq!(paras(&f).concat(), "Body");
    assert_eq!(f.meta.title, "Hidden title");
}
