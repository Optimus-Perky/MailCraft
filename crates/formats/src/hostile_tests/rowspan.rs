//! Upstream #1235: a table of rowspans multiplied into rows × rowspan cells.

use std::time::{Duration, Instant};

use wordcraft_doc::Block;

use crate::html::import;
use crate::model::{Cell, FTable};

/// Generous: the fixed importer needs a few milliseconds for each input below.
const BOUND: Duration = Duration::from_secs(3);

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    let took = t.elapsed();
    assert!(took < BOUND, "{what} took {took:?} (bound {BOUND:?})");
    out
}

/// Many rows, each spanning 1 000 rows down, asked for rows × 1 000 covered cells.
#[test]
fn rowspan_expansion_is_bounded() {
    let mut t = FTable { rows: (0..40_000).map(|_| vec![Cell { rowspan: 1000, ..Default::default() }]).collect(), widths: Vec::new() };
    timed("insert_covered on 40 000 rows of rowspan 1000", || t.insert_covered());
    assert!(t.rows.iter().all(|r| r.len() <= 64), "a row grew past the column cap");
    let html = format!("<table>{}</table>", "<tr><td rowspan=1000>x</td></tr>".repeat(40_000));
    let d = timed("a 40 000-row table of rowspan 1000", || import(&html));
    let cells: usize = d
        .body
        .iter()
        .map(|b| match &**b {
            Block::Table(t) => t.rows.iter().map(|r| r.cells.len()).sum(),
            _ => 0,
        })
        .sum();
    assert!(cells <= 40_000 + (1 << 17), "{cells} cells from a 1.3 MB table");
}
