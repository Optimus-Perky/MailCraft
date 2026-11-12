//! Hostile mail on the UI thread: opening a message builds its reader (`html::import`, then
//! layout) and replying builds an editor from it (import, proofing, layout). A crafted message
//! must not freeze the window. The pieces are what broke WordCraft v0.3.0's importer (ported
//! fixes: upstream #355, #1235) next to what it already bounded (nesting, attributes, unclosed
//! tags), so a regression in any of them shows.

use std::time::{Duration, Instant};

/// Far above what the fixed path needs (see the printed timings), far below what v0.3.0 took.
const BOUND: Duration = Duration::from_secs(10);

/// The parts of the crafted message, each a few hundred KB.
fn pieces() -> Vec<(&'static str, String)> {
    vec![
        ("deep <div> nesting", format!("{}deep", "<div>".repeat(40_000))),
        ("deep inline nesting", format!("{}deep", "<b><i><u><span style='color:red'>".repeat(10_000))),
        ("huge attribute run", format!("<p {}>attrs</p>", "a=1 ".repeat(50_000))),
        ("huge style run", format!("<p style=\"{}\">style</p>", "color:red;margin:1pt 2pt;".repeat(10_000))),
        ("unclosed tags", "<p><b><i><u><font size=7><table><tr><td><a href=x>".repeat(5_000)),
        ("stray ampersands", format!("<p>{}</p>", "&#".repeat(100_000))),
        ("thousands of runs", format!("<p>{}</p>", "<b>x</b><i>y</i>".repeat(10_000))),
        ("thousands of anchors", format!("<p>{}</p>", (0..5_000).map(|i| format!("<a id=a{i}>x</a>")).collect::<String>())),
        ("rowspan table", format!("<table>{}</table>", "<tr><td rowspan=1000>x</td></tr>".repeat(5_000))),
        ("unbreakable word", format!("<p>{}</p>", "w".repeat(200_000))),
    ]
}

fn open(html: &str) -> (Duration, Duration) {
    let t = Instant::now();
    let mut s = crate::reader_session(html);
    let import = t.elapsed();
    s.view.web_width = 600.0;
    let _ = s.layout();
    (import, t.elapsed() - import)
}

fn reply(html: &str) -> Duration {
    let t = Instant::now();
    let mut e = crate::editor(html, 2);
    e.session.view.web_width = 600.0;
    let _ = e.session.layout();
    t.elapsed()
}

#[test]
fn crafted_message_opens_and_replies_in_bounded_time() {
    let mut all = String::new();
    for (name, html) in pieces() {
        let (i, l) = open(&html);
        println!("hostile open: {name:22} {:>8} bytes  import {:>9.3?}  layout {:>9.3?}", html.len(), i, l);
        all.push_str(&html);
        // Each piece closes what it can, so the next one starts on a clean line where it can.
        all.push_str(&"</a></td></tr></table></font></span></u></i></b></p></div>".repeat(300));
    }
    let (i, l) = open(&all);
    println!("hostile open: {:22} {:>8} bytes  import {:>9.3?}  layout {:>9.3?}", "whole message", all.len(), i, l);
    let r = reply(&all);
    println!("hostile reply: whole message editor + proofing layout {r:.3?}");
    assert!(i + l < BOUND, "opening the crafted message took {:?}", i + l);
    assert!(r < BOUND, "replying to the crafted message took {r:?}");
}
