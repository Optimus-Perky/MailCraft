//! Upstream #2257: raw-text elements end only at their own end tag.

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

/// `</scripted>` inside a script is not `</script>`: what follows it is still script.
#[test]
fn raw_text_elements_skip_only_their_own_end_tag() {
    let f = parse("<script>var a = \"</scripted>\"; var b = \"<b>malicious</b>\";</script><p>Hello</p>");
    assert_eq!(paras(&f), vec!["Hello".to_string()]);
    let f = parse("<style>/* </styles> */ p { color: red; }</style><p>World</p>");
    assert_eq!(paras(&f), vec!["World".to_string()]);
    let f = parse("<textarea></textareax>hidden</TEXTAREA ><p>Shown</p>");
    assert_eq!(paras(&f), vec!["Shown".to_string()]);
}
