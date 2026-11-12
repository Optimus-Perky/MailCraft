//! Upstream #355: hostile HTML took quadratic time.

use std::time::{Duration, Instant};

use wordcraft_doc::{Block, Document, Paragraph};

use crate::html::{import, parse, unescape};
use crate::model::{FBlock, Flow, Fmt};

/// Generous: the fixed importer needs a few milliseconds for each input below.
const BOUND: Duration = Duration::from_secs(3);

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    let took = t.elapsed();
    assert!(took < BOUND, "{what} took {took:?} (bound {BOUND:?})");
    out
}

fn paras(f: &Flow) -> Vec<String> {
    f.blocks
        .iter()
        .filter_map(|b| match b {
            FBlock::Para(p) => Some(p.text()),
            _ => None,
        })
        .collect()
}

fn body_paragraphs(d: &Document) -> Vec<&Paragraph> {
    d.body
        .iter()
        .filter_map(|b| match &**b {
            Block::Para(p) => Some(p),
            _ => None,
        })
        .collect()
}

/// Every stray `&` made `unescape` search the whole rest of the text for a `;`.
#[test]
fn stray_ampersands_take_linear_time() {
    let html = format!("<p>{}</p>", "&#".repeat(300_000));
    let f = timed("300 000 stray `&#`", || parse(&html));
    assert_eq!(paras(&f).concat().len(), 600_000);
    // Real references still read.
    assert_eq!(unescape("a &amp; b &lt;c&gt; &#65;&#x42; &bogus; &"), "a & b <c> AB &bogus; &");
}

/// Each inline went through `Paragraph::insert_text`, which rescans every earlier run.
#[test]
fn thousands_of_runs_take_linear_time() {
    let html = format!("<p>{}</p>", "<b>x</b><i>y</i>".repeat(12_500));
    let d = timed("25 000 runs", || import(&html));
    let p = body_paragraphs(&d);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].text.len(), 25_000);
    assert_eq!(p[0].runs.len(), 25_000, "alternating bold and italic stay separate runs");
    assert_eq!(p[0].runs.iter().map(|r| r.len).sum::<usize>(), p[0].text.len());
}

/// Each object went through `insert_object`, which counts the objects before it from the start.
#[test]
fn thousands_of_anchors_take_linear_time() {
    let html = format!("<p>{}</p>", (0..30_000).map(|i| format!("<a id=a{i}>x</a>")).collect::<String>());
    let d = timed("30 000 anchors", || import(&html));
    let p = body_paragraphs(&d);
    assert_eq!(p[0].objects.len(), 60_000, "a bookmark start and end for each");
    assert_eq!(p[0].text.chars().filter(|c| *c == 'x').count(), 30_000);
}

/// Appending runs directly must give the same paragraph `insert_text` gave.
#[test]
fn runs_appended_directly_match_insert_text() {
    let d = import("<p>a<b>b</b><b>c</b><i>d</i>e</p>");
    let p = body_paragraphs(&d)[0].clone();
    let mut want = Paragraph::new();
    let cp = |bold: bool, italic: bool| crate::model::char_props(&Fmt { bold, italic, ..Default::default() });
    for (t, cp) in [("a", cp(false, false)), ("b", cp(true, false)), ("c", cp(true, false)), ("d", cp(false, true)), ("e", cp(false, false))] {
        let end = want.len();
        let _ = want.insert_text(end, t, &cp);
    }
    assert_eq!(p.text, want.text);
    assert_eq!(p.runs, want.runs);
}
