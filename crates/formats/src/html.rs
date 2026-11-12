//! HTML.
//!
//! Import is a tolerant tag-soup reader (no DOM): a tokenizer plus a stack of open elements with
//! HTML's implicit closes for `p`, `li`, `tr`, `td`/`th` and headings. It understands `p`, `div`,
//! `h1`–`h6`, `b`/`strong`, `i`/`em`, `u`, `s`/`strike`/`del`, `sup`/`sub`, `code`/`kbd`, `a`
//! (links and `id`/`name` anchors), `br`, `hr`, `ul`/`ol`/`li` (nested), `table`/`tr`/`td`/`th`
//! (colspan/rowspan), `img` with `data:` URIs, `blockquote`, `pre`, `font`, and the CSS
//! properties `color`, `background-color`, `font-weight`, `font-style`, `font-size`,
//! `font-family`, `text-decoration`, `vertical-align`, `text-align`, `white-space` and page
//! breaks. Scripts and styles are skipped; nesting is capped.
//!
//! Export writes clean semantic HTML with inline CSS and pictures as `data:` URIs.

use wordcraft_doc::props::{Border, BorderStyle, Borders, LineSpacing, Underline};
use wordcraft_doc::section::NumFormat;
use wordcraft_doc::{Align, Document, Rgb};

use crate::model::{
    self, Cell, FBlock, FTable, Flow, Fmt, Inline, Kind, ListFmt, ListInfo, Meta, Para, base64_encode, data_uri, make_img, mime_of, parse_color,
};

/// Open elements kept at most (deeper start tags are ignored).
const MAX_STACK: usize = 256;

// ---------------------------------------------------------------------------------------------
// Entities and decoding

/// Decode an entity body (`amp`, `#38`, `#x26`) to its text.
pub fn decode_entity(name: &str) -> Option<String> {
    if let Some(num) = name.strip_prefix('#') {
        let v = if let Some(h) = num.strip_prefix(['x', 'X']) { u32::from_str_radix(h, 16).ok()? } else { num.parse::<u32>().ok()? };
        let c = match v {
            0 => '\u{FFFD}',
            0x80..=0x9F => crate::txt::cp1252(v as u8),
            _ => char::from_u32(v).unwrap_or('\u{FFFD}'),
        };
        return Some(c.to_string());
    }
    let c = match name {
        "amp" | "AMP" => '&',
        "lt" | "LT" => '<',
        "gt" | "GT" => '>',
        "quot" | "QUOT" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "shy" => '\u{00AD}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "sbquo" => '‚',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "sect" => '§',
        "para" => '¶',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "dagger" => '†',
        "Dagger" => '‡',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "harr" => '↔',
        "le" => '≤',
        "ge" => '≥',
        "ne" => '≠',
        "infin" => '∞',
        "check" => '✓',
        "iexcl" => '¡',
        "iquest" => '¿',
        "szlig" => 'ß',
        "auml" => 'ä',
        "ouml" => 'ö',
        "uuml" => 'ü',
        "Auml" => 'Ä',
        "Ouml" => 'Ö',
        "Uuml" => 'Ü',
        "eacute" => 'é',
        "egrave" => 'è',
        "ecirc" => 'ê',
        "Eacute" => 'É',
        "aacute" => 'á',
        "agrave" => 'à',
        "acirc" => 'â',
        "iacute" => 'í',
        "oacute" => 'ó',
        "uacute" => 'ú',
        "ntilde" => 'ñ',
        "Ntilde" => 'Ñ',
        "ccedil" => 'ç',
        "Ccedil" => 'Ç',
        "aring" => 'å',
        "Aring" => 'Å',
        "oslash" => 'ø',
        "Oslash" => 'Ø',
        "aelig" => 'æ',
        "AElig" => 'Æ',
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "pi" => 'π',
        "sigma" => 'σ',
        "mu" => 'μ',
        "Omega" => 'Ω',
        "omega" => 'ω',
        "zwj" => '\u{200D}',
        "zwnj" => '\u{200C}',
        _ => return None,
    };
    Some(c.to_string())
}

/// Replace entity references in `s`.
pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(k) = rest.find('&') {
        out.push_str(&rest[..k]);
        let after = &rest[k + 1..];
        let end = after.find(';').filter(|e| *e <= 32);
        match end.and_then(|e| decode_entity(&after[..e]).map(|t| (t, e))) {
            Some((t, e)) => {
                out.push_str(&t);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decode HTML bytes: BOM, then a `charset` in the first 2 KB, else UTF-8 with a Windows-1252
/// fallback.
pub fn decode(b: &[u8]) -> String {
    if b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]) || b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return crate::txt::decode(b);
    }
    let head = String::from_utf8_lossy(b.get(..b.len().min(2048)).unwrap_or(b)).to_ascii_lowercase();
    let latin = head.contains("charset=windows-1252") || head.contains("charset=iso-8859-1") || head.contains("charset=\"windows-1252\"");
    if !latin && let Ok(s) = std::str::from_utf8(b) {
        return s.to_string();
    }
    crate::txt::decode(b)
}

// ---------------------------------------------------------------------------------------------
// Tokenizer

#[derive(Debug)]
enum Tok {
    Text(String),
    Start { name: String, attrs: Vec<(String, String)>, self_close: bool },
    End(String),
}

struct Lexer<'a> {
    s: &'a str,
    pos: usize,
}

impl Lexer<'_> {
    fn rest(&self) -> &str {
        self.s.get(self.pos..).unwrap_or("")
    }

    /// Skip past the next occurrence of `pat` (ASCII case-insensitive), or to the end.
    fn skip_past(&mut self, pat: &str) -> usize {
        let start = self.pos;
        let hay = self.rest().as_bytes();
        let p = pat.as_bytes();
        let found = hay.windows(p.len().max(1)).position(|w| w.eq_ignore_ascii_case(p));
        match found {
            Some(k) => self.pos += k + p.len(),
            None => self.pos = self.s.len(),
        }
        found.map(|k| start + k).unwrap_or(self.s.len())
    }

    fn next(&mut self) -> Option<Tok> {
        let rest = self.rest();
        if rest.is_empty() {
            return None;
        }
        if !rest.starts_with('<') {
            let end = rest.find('<').unwrap_or(rest.len());
            let t = unescape(&rest[..end]);
            self.pos += end;
            return Some(Tok::Text(t));
        }
        let b = rest.as_bytes();
        if rest.starts_with("<!--") {
            self.pos += 4;
            self.skip_past("-->");
            return Some(Tok::Text(String::new()));
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            self.skip_past(">");
            return Some(Tok::Text(String::new()));
        }
        if rest.starts_with("</") {
            let name: String =
                rest.strip_prefix("</").unwrap_or("").chars().take_while(|c| c.is_ascii_alphanumeric() || *c == ':' || *c == '-').collect();
            if name.is_empty() {
                self.pos += 2;
                return Some(Tok::Text("</".into()));
            }
            self.skip_past(">");
            return Some(Tok::End(name.to_ascii_lowercase()));
        }
        if !b.get(1).is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
            return Some(Tok::Text("<".into()));
        }
        // Start tag.
        let name: String = rest[1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == ':' || *c == '-').collect();
        let mut i = 1 + name.len();
        let mut attrs = Vec::new();
        let mut self_close = false;
        loop {
            while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                i += 1;
            }
            match b.get(i) {
                None => break,
                Some(b'>') => {
                    i += 1;
                    break;
                }
                Some(b'/') => {
                    if b.get(i + 1) == Some(&b'>') {
                        self_close = true;
                        i += 2;
                        break;
                    }
                    i += 1;
                    continue;
                }
                _ => {}
            }
            let a0 = i;
            while b.get(i).is_some_and(|c| !c.is_ascii_whitespace() && !matches!(c, b'=' | b'>' | b'/')) {
                i += 1;
            }
            let an = rest.get(a0..i).unwrap_or("").to_ascii_lowercase();
            if an.is_empty() {
                i += 1;
                continue;
            }
            while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                i += 1;
            }
            let mut val = String::new();
            if b.get(i) == Some(&b'=') {
                i += 1;
                while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                    i += 1;
                }
                match b.get(i) {
                    Some(&q) if q == b'"' || q == b'\'' => {
                        let v0 = i + 1;
                        let e = rest.get(v0..).and_then(|r| r.find(q as char)).map(|k| v0 + k).unwrap_or(rest.len());
                        val = unescape(rest.get(v0..e).unwrap_or(""));
                        i = e + 1;
                    }
                    _ => {
                        let v0 = i;
                        while b.get(i).is_some_and(|c| !c.is_ascii_whitespace() && *c != b'>') {
                            i += 1;
                        }
                        val = unescape(rest.get(v0..i).unwrap_or(""));
                    }
                }
            }
            if attrs.len() < 64 {
                attrs.push((an, val));
            }
        }
        self.pos += i.min(rest.len());
        Some(Tok::Start { name: name.to_ascii_lowercase(), attrs, self_close })
    }
}

// ---------------------------------------------------------------------------------------------
// Builder

#[derive(Clone, Copy, PartialEq, Debug)]
enum ElKind {
    Inline,
    Block,
    Heading(u8),
    Quote,
    Pre,
    List(bool),
    Li,
    Table,
    Tr,
    Cell,
    Head,
}

#[derive(Clone, Debug)]
struct El {
    name: String,
    kind: ElKind,
    fmt: Fmt,
    align: Option<Align>,
    preserve: bool,
    page_break: bool,
    /// Li: a paragraph of this item was already emitted.
    used: bool,
    /// Paragraph formatting from this block element's `style`.
    block: BlockCss,
    /// Lists written by WordCraft: the numbering they spell out (`data-mc-list`).
    list_fmt: Option<ListFmt>,
    /// Text inside is a list marker WordCraft wrote out (`data-mc-label`): not content.
    skip: bool,
}

/// Paragraph formatting a block element's `style` carries.
#[derive(Clone, Debug, Default)]
struct BlockCss {
    indent_left: Option<f32>,
    indent_right: Option<f32>,
    indent_first: Option<f32>,
    space_before: Option<f32>,
    space_after: Option<f32>,
    line: Option<LineSpacing>,
    shading: Option<Rgb>,
    borders: Option<Borders>,
}

struct TableB {
    rows: Vec<Vec<Cell>>,
    row: Option<Vec<Cell>>,
}

struct Builder {
    containers: Vec<Vec<FBlock>>,
    cells: Vec<Cell>,
    tables: Vec<TableB>,
    stack: Vec<El>,
    para: Option<Para>,
    /// The current paragraph continues a list item (joins the previous paragraph).
    para_cont: bool,
    meta: Meta,
    title: Option<String>,
}

fn is_void(n: &str) -> bool {
    matches!(
        n,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
            | "basefont"
            | "frame"
    )
}

fn block_kind(n: &str) -> Option<ElKind> {
    Some(match n {
        "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "nav" | "aside" | "figure" | "figcaption" | "address" | "dl" | "dt"
        | "dd" | "center" | "form" | "fieldset" | "body" | "html" | "caption" | "thead" | "tbody" | "tfoot" | "details" | "summary" | "legend"
        | "hgroup" => ElKind::Block,
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => ElKind::Heading(n.get(1..2).and_then(|d| d.parse().ok()).unwrap_or(1)),
        "blockquote" => ElKind::Quote,
        "pre" | "listing" | "xmp" | "plaintext" => ElKind::Pre,
        "ul" | "menu" | "dir" => ElKind::List(false),
        "ol" => ElKind::List(true),
        "li" => ElKind::Li,
        "table" => ElKind::Table,
        "tr" => ElKind::Tr,
        "td" | "th" => ElKind::Cell,
        "head" => ElKind::Head,
        _ => return None,
    })
}

/// Closes an open `p` (HTML's "close a p element" on block starts).
fn closes_p(n: &str) -> bool {
    matches!(
        n,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "div"
            | "dl"
            | "fieldset"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "ul"
            | "figure"
            | "center"
            | "dd"
            | "dt"
            | "li"
    )
}

fn css_len_pt(v: &str) -> Option<f32> {
    let v = v.trim().to_ascii_lowercase();
    let num = |s: &str| s.trim().parse::<f32>().ok().filter(|x| x.is_finite());
    if let Some(x) = v.strip_suffix("pt") {
        num(x)
    } else if let Some(x) = v.strip_suffix("px") {
        num(x).map(|p| p * 0.75)
    } else if let Some(x) = v.strip_suffix("rem").or_else(|| v.strip_suffix("em")) {
        num(x).map(|p| p * 12.0)
    } else if let Some(x) = v.strip_suffix("in") {
        num(x).map(|p| p * 72.0)
    } else if let Some(x) = v.strip_suffix("cm") {
        num(x).map(|p| p * 72.0 / 2.54)
    } else if let Some(x) = v.strip_suffix("mm") {
        num(x).map(|p| p * 72.0 / 25.4)
    } else if let Some(x) = v.strip_suffix('%') {
        num(x).map(|p| p * 0.12)
    } else {
        match v.as_str() {
            "xx-small" => Some(7.0),
            "x-small" => Some(7.5),
            "small" => Some(10.0),
            "medium" => Some(12.0),
            "large" => Some(13.5),
            "x-large" => Some(18.0),
            "xx-large" => Some(24.0),
            _ => num(&v).map(|p| p * 0.75),
        }
    }
}

/// Text shadows the exporter writes for Word's shadow, emboss and engrave effects.
const SHADOW_CSS: &str = "1px 1px 2px #808080";
const EMBOSS_CSS: &str = "-1px -1px 0 #ffffff,1px 1px 0 #7f7f7f";
const ENGRAVE_CSS: &str = "1px 1px 0 #ffffff,-1px -1px 0 #7f7f7f";

/// Apply a `style` attribute to formatting / block properties. `strike_el`: the element is
/// `s`/`strike`/`del`, so a decoration style (double) belongs to the strikethrough.
fn apply_style(css: &str, f: &mut Fmt, align: &mut Option<Align>, preserve: &mut bool, page_break: &mut bool, strike_el: bool) {
    let mut deco_style: Option<String> = None;
    let mut deco_lines = String::new();
    for decl in css.split(';').take(64) {
        let Some((k, v)) = decl.split_once(':') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().trim_end_matches("!important").trim();
        let lv = v.to_ascii_lowercase();
        match k.as_str() {
            "font-weight" => f.bold = lv.starts_with("bold") || lv.parse::<u32>().is_ok_and(|w| w >= 600),
            "font-style" => f.italic = lv == "italic" || lv == "oblique",
            "text-decoration" | "text-decoration-line" => {
                if lv.contains("none") {
                    f.underline = false;
                    f.strike = false;
                    f.double_strike = false;
                    f.underline_style = None;
                }
                if lv.contains("underline") {
                    f.underline = true;
                }
                if lv.contains("line-through") {
                    f.strike = true;
                }
                deco_lines.push_str(&lv);
                if k == "text-decoration" {
                    for w in ["double", "dotted", "dashed", "wavy", "solid"] {
                        if lv.split_whitespace().any(|x| x == w) {
                            deco_style = Some(w.to_string());
                        }
                    }
                }
            }
            "text-decoration-style" => deco_style = Some(lv.clone()),
            "text-decoration-thickness" => {
                if css_len_pt(v).is_some_and(|t| t >= 1.4) || lv == "thick" {
                    deco_style.get_or_insert_with(|| "thick".into());
                }
            }
            "text-decoration-color" => f.underline_color = parse_color(v),
            "text-transform" => f.caps = lv == "uppercase",
            "font-variant" | "font-variant-caps" => f.small_caps = lv.contains("small-caps"),
            "letter-spacing" => f.spacing = css_len_pt(v).filter(|x| *x != 0.0 && x.abs() <= 100.0),
            "text-shadow" => {
                let n: String = lv.chars().filter(|c| !c.is_whitespace()).collect();
                let norm = |c: &str| c.chars().filter(|c| !c.is_whitespace()).collect::<String>();
                f.emboss = n.contains(&norm(EMBOSS_CSS));
                f.engrave = n.contains(&norm(ENGRAVE_CSS));
                f.shadow = n != "none" && (n.contains(&norm(SHADOW_CSS)) || (!f.emboss && !f.engrave && !n.is_empty()));
            }
            "-webkit-text-stroke" | "-webkit-text-stroke-width" => f.outline = !lv.starts_with('0') && lv != "none",
            "color" => {
                if let Some(c) = parse_color(v) {
                    f.color = Some(c);
                }
            }
            "background-color" | "background" => {
                if let Some(c) = v.split_whitespace().find_map(parse_color) {
                    f.background = Some(c);
                }
            }
            "font-size" => {
                if let Some(p) = css_len_pt(v).filter(|p| *p >= 1.0 && *p <= 1638.0) {
                    f.size = Some(p);
                }
            }
            "font-family" => {
                let fam = v.split(',').next().unwrap_or("").trim().trim_matches(['"', '\'']).to_string();
                if model::is_mono(&fam) || fam.eq_ignore_ascii_case("monospace") {
                    f.code = true;
                } else if !fam.is_empty() && !matches!(fam.to_ascii_lowercase().as_str(), "serif" | "sans-serif" | "inherit" | "initial") {
                    f.font = Some(fam);
                }
            }
            "vertical-align" => {
                f.sup = lv == "super";
                f.sub = lv == "sub";
                // A raised / lowered position (`vertical-align:3pt`).
                f.position = if f.sup || f.sub { None } else { css_len_pt(v).filter(|x| *x != 0.0 && !lv.ends_with('%')) };
            }
            "text-align" => {
                *align = match lv.as_str() {
                    "center" => Some(Align::Center),
                    "right" | "end" => Some(Align::Right),
                    "justify" => Some(Align::Justify),
                    "left" | "start" => Some(Align::Left),
                    _ => *align,
                }
            }
            "white-space" => *preserve = lv.starts_with("pre"),
            "page-break-before" | "break-before" => *page_break = lv == "always" || lv == "page",
            _ => {}
        }
    }
    if let Some(st) = deco_style {
        let strike = strike_el || (deco_lines.contains("line-through") && !deco_lines.contains("underline"));
        if strike {
            f.double_strike = st == "double";
        } else {
            f.underline_style = match st.as_str() {
                "double" => Some(Underline::Double),
                "dotted" => Some(Underline::Dotted),
                "dashed" => Some(Underline::Dash),
                "wavy" => Some(Underline::Wave),
                "thick" => Some(Underline::Thick),
                _ => None,
            };
        }
    }
}

/// One CSS border value (`1pt solid #c00000`) as a paragraph border; `None` when it draws nothing.
fn css_border(v: &str) -> Option<Border> {
    let v = v.replace(", ", ",");
    let mut b = Border { style: BorderStyle::None, width: 0.75, color: None, space: 0.0 };
    for t in v.split_whitespace().take(8) {
        let lt = t.to_ascii_lowercase();
        match lt.as_str() {
            "none" | "hidden" => return None,
            "solid" | "groove" | "ridge" | "inset" | "outset" => b.style = BorderStyle::Single,
            "double" => b.style = BorderStyle::Double,
            "dotted" => b.style = BorderStyle::Dotted,
            "dashed" => b.style = BorderStyle::Dashed,
            "thin" => b.width = 0.75,
            "medium" => b.width = 2.25,
            "thick" => b.width = 3.75,
            "windowtext" | "currentcolor" | "auto" => b.color = None,
            _ if lt.starts_with(|c: char| c.is_ascii_digit() || c == '.') => {
                if let Some(w) = css_len_pt(&lt) {
                    b.width = w;
                }
            }
            _ => {
                if let Some(c) = parse_color(t) {
                    b.color = Some(c);
                }
            }
        }
    }
    if b.style == BorderStyle::Double {
        // Written three times as wide so both lines show.
        b.width /= 3.0;
    }
    (b.style != BorderStyle::None && b.width > 0.0 && b.width.is_finite()).then(|| Border { width: b.width.clamp(0.25, 6.0), ..b })
}

/// One to four CSS box values (top, right, bottom, left).
fn css_box(v: &str) -> [Option<f32>; 4] {
    let vals: Vec<Option<f32>> = v.split_whitespace().take(4).map(|x| if x.ends_with('%') || x == "auto" { None } else { css_len_pt(x) }).collect();
    let g = |i: usize| vals.get(i).copied().flatten();
    match vals.len() {
        1 => [g(0), g(0), g(0), g(0)],
        2 => [g(0), g(1), g(0), g(1)],
        3 => [g(0), g(1), g(2), g(1)],
        4 => [g(0), g(1), g(2), g(3)],
        _ => [None; 4],
    }
}

/// Paragraph formatting from a block element's `style`.
fn block_css(css: &str) -> BlockCss {
    let mut b = BlockCss::default();
    let mut line: Option<String> = None;
    let mut exact = false;
    let mut sides: [Option<Border>; 4] = [None; 4];
    let mut any_border = false;
    let mut pad: [Option<f32>; 4] = [None; 4];
    let len = |v: &str| if v.trim().ends_with('%') || v.trim() == "auto" { None } else { css_len_pt(v) };
    for decl in css.split(';').take(64) {
        let Some((k, v)) = decl.split_once(':') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().trim_end_matches("!important").trim();
        match k.as_str() {
            "margin" => {
                let [t, r, bt, l] = css_box(v);
                b.space_before = t.map(|x| x.max(0.0));
                b.indent_right = r;
                b.space_after = bt.map(|x| x.max(0.0));
                b.indent_left = l;
            }
            "margin-top" => b.space_before = len(v).map(|x| x.max(0.0)),
            "margin-bottom" => b.space_after = len(v).map(|x| x.max(0.0)),
            "margin-left" => b.indent_left = len(v),
            "margin-right" => b.indent_right = len(v),
            "text-indent" => b.indent_first = len(v),
            "line-height" => line = Some(v.to_ascii_lowercase()),
            "mso-line-height-rule" => exact = v.eq_ignore_ascii_case("exactly"),
            "background-color" | "background" => {
                if let Some(c) = v.split_whitespace().find_map(parse_color) {
                    b.shading = Some(c);
                }
            }
            "border" => {
                any_border = true;
                sides = [css_border(v); 4];
            }
            "border-top" | "border-right" | "border-bottom" | "border-left" => {
                any_border = true;
                let i = ["border-top", "border-right", "border-bottom", "border-left"].iter().position(|x| *x == k).unwrap_or(0);
                if let Some(s) = sides.get_mut(i) {
                    *s = css_border(v);
                }
            }
            "padding" => pad = css_box(v),
            "padding-top" => pad[0] = len(v),
            "padding-right" => pad[1] = len(v),
            "padding-bottom" => pad[2] = len(v),
            "padding-left" => pad[3] = len(v),
            _ => {}
        }
    }
    if let Some(l) = line {
        let num = |s: &str| s.trim().parse::<f32>().ok().filter(|x| x.is_finite() && *x > 0.0);
        b.line = if l == "normal" {
            Some(LineSpacing::Multiple(1.0))
        } else if let Some(pc) = l.strip_suffix('%') {
            num(pc).map(|x| LineSpacing::Multiple((x / 100.0).clamp(0.25, 10.0)))
        } else if let Some(m) = num(&l) {
            Some(LineSpacing::Multiple(m.clamp(0.25, 10.0)))
        } else {
            css_len_pt(&l).filter(|x| *x > 0.0).map(|x| if exact { LineSpacing::Exactly(x) } else { LineSpacing::AtLeast(x) })
        };
    }
    if any_border {
        let with_pad = |s: Option<Border>, p: Option<f32>| s.map(|x| Border { space: p.unwrap_or(0.0).clamp(0.0, 31.0), ..x });
        let bs = Borders {
            top: with_pad(sides[0], pad[0]),
            right: with_pad(sides[1], pad[1]),
            bottom: with_pad(sides[2], pad[2]),
            left: with_pad(sides[3], pad[3]),
            ..Default::default()
        };
        if bs != Borders::default() {
            b.borders = Some(bs);
        }
    }
    b
}

/// A list's numbering as WordCraft writes it in `data-mc-list`: `decimal:%1.|lowerLetter:%2)`.
fn parse_list_levels(v: &str) -> Option<Vec<(NumFormat, String)>> {
    let levels: Vec<(NumFormat, String)> = v
        .split('|')
        .take(9)
        .filter_map(|l| l.split_once(':'))
        .map(|(f, t)| (NumFormat::from_ooxml(f.trim()), t.chars().take(32).collect()))
        .collect();
    (!levels.is_empty()).then_some(levels)
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

impl Builder {
    fn new() -> Builder {
        Builder {
            containers: vec![Vec::new()],
            cells: Vec::new(),
            tables: Vec::new(),
            stack: Vec::new(),
            para: None,
            para_cont: false,
            meta: Meta::default(),
            title: None,
        }
    }

    fn fmt(&self) -> Fmt {
        self.stack.last().map(|e| e.fmt.clone()).unwrap_or_default()
    }

    fn preserve(&self) -> bool {
        self.stack.last().is_some_and(|e| e.preserve)
    }

    fn in_head(&self) -> bool {
        self.stack.iter().any(|e| e.kind == ElKind::Head)
    }

    /// Paragraph context from the open elements (innermost first, up to the enclosing cell).
    fn start_para(&mut self) -> &mut Para {
        if self.para.is_none() {
            let mut p = Para::default();
            let mut list_level: Option<(bool, usize)> = None;
            let mut li_at: Option<usize> = None;
            let mut lists = 0usize;
            // The innermost block element's paragraph formatting; borders of an enclosing box
            // (a `div` round several paragraphs); the nearest block shading; the list's numbering.
            let mut own: Option<&BlockCss> = None;
            let mut group: Option<Borders> = None;
            let mut shading: Option<Rgb> = None;
            let mut list_fmt: Option<&ListFmt> = None;
            for (i, e) in self.stack.iter().enumerate().rev() {
                if !matches!(e.kind, ElKind::Inline | ElKind::List(_) | ElKind::Cell | ElKind::Table | ElKind::Tr | ElKind::Head) {
                    if own.is_none() {
                        own = Some(&e.block);
                    } else if group.is_none() && e.kind == ElKind::Block && e.block.borders.is_some() {
                        group = e.block.borders;
                    }
                    if shading.is_none() {
                        shading = e.block.shading;
                    }
                }
                if matches!(e.kind, ElKind::List(_)) && list_fmt.is_none() {
                    list_fmt = e.list_fmt.as_ref();
                }
                match e.kind {
                    ElKind::Cell | ElKind::Table => break,
                    ElKind::Heading(n) if p.kind == Kind::Normal => p.kind = Kind::Heading(n.clamp(1, 6)),
                    ElKind::Pre if p.kind == Kind::Normal => p.kind = Kind::Code,
                    ElKind::Quote if p.kind == Kind::Normal => p.kind = Kind::Quote,
                    ElKind::Li if li_at.is_none() => li_at = Some(i),
                    ElKind::List(o) => {
                        if list_level.is_none() {
                            list_level = Some((o, 0));
                        }
                        lists += 1;
                    }
                    _ => {}
                }
                if p.align.is_none() && e.align.is_some() && e.kind != ElKind::Inline {
                    p.align = e.align;
                }
            }
            if let Some(b) = own {
                p.space_before = b.space_before;
                p.space_after = b.space_after;
                p.line_spacing = b.line;
                if list_level.is_none() || li_at.is_none() {
                    // List items are indented by their nesting.
                    p.indent_left = b.indent_left;
                    p.indent_right = b.indent_right;
                    p.indent_first = b.indent_first;
                }
                p.borders = match group {
                    // One box round a run of paragraphs; a top border inside it is the line between.
                    Some(g) => Some(Borders { between: g.between.or(b.borders.and_then(|x| x.top)), ..g }),
                    None => b.borders,
                };
            }
            p.shading = shading;
            if let (Some((o, _)), Some(li)) = (list_level, li_at) {
                p.list_fmt = list_fmt.cloned();
                p.list = Some(ListInfo { ordered: o, level: lists.saturating_sub(1).min(8) as u8 });
                self.para_cont = self.stack.get(li).is_some_and(|e| e.used);
                if let Some(e) = self.stack.get_mut(li) {
                    e.used = true;
                }
            } else {
                self.para_cont = false;
            }
            p.page_break = self.stack.iter().rev().take_while(|e| e.kind != ElKind::Cell).any(|e| e.page_break);
            self.para = Some(p);
        }
        self.para.get_or_insert_with(Para::default)
    }

    fn container(&mut self) -> &mut Vec<FBlock> {
        if self.containers.is_empty() {
            self.containers.push(Vec::new());
        }
        let n = self.containers.len() - 1;
        &mut self.containers[n]
    }

    fn flush(&mut self) {
        let Some(mut p) = self.para.take() else { return };
        if p.kind != Kind::Code {
            p.trim();
        }
        if p.inlines.len() == 1 && p.text() == "\u{00A0}" {
            // `<p>&nbsp;</p>`: an empty paragraph on purpose.
            p.inlines.clear();
            self.para_cont = false;
            self.container().push(FBlock::Para(p));
            return;
        }
        if p.is_empty() && p.kind != Kind::Code && p.list.is_none() {
            // Whitespace-only text between blocks.
            return;
        }
        let cont = std::mem::take(&mut self.para_cont);
        let level = p.list.map(|l| l.level);
        let c = self.container();
        if cont
            && let Some(FBlock::Para(prev)) = c.last_mut()
            && prev.list.is_some()
            && prev.list.map(|l| l.level) == level
        {
            prev.push_text("\n", &Fmt::default());
            prev.inlines.extend(p.inlines);
            return;
        }
        c.push(FBlock::Para(p));
    }

    fn text(&mut self, t: &str) {
        if self.stack.iter().any(|e| e.skip) {
            return;
        }
        if self.in_head() {
            if let Some(title) = &mut self.title {
                title.push_str(t);
            }
            return;
        }
        if t.is_empty() {
            return;
        }
        let inside_table_only = matches!(
            self.stack.iter().rev().find(|e| matches!(e.kind, ElKind::Table | ElKind::Tr | ElKind::Cell)).map(|e| e.kind),
            Some(ElKind::Table | ElKind::Tr)
        );
        if inside_table_only && t.trim().is_empty() {
            return;
        }
        let code = self.stack.iter().any(|e| e.kind == ElKind::Pre);
        let f = self.fmt();
        if code {
            let mut first = true;
            for line in t.split('\n') {
                if !first {
                    // A newline right after <pre> is ignored.
                    let empty_start = self.para.as_ref().is_none_or(|p| p.inlines.is_empty()) && self.pre_fresh();
                    if !empty_start {
                        self.start_para();
                        self.flush();
                    }
                }
                first = false;
                let line = line.trim_end_matches('\r');
                if !line.is_empty() {
                    self.start_para().push_text(line, &Fmt { code: false, ..f.clone() });
                    self.mark_pre_used();
                }
            }
            return;
        }
        if self.preserve() {
            self.start_para().push_text(t, &f);
            return;
        }
        // Collapse whitespace.
        let mut s = String::with_capacity(t.len());
        let mut prev_space = self.para.as_ref().is_none_or(|p| match p.inlines.last() {
            Some(Inline::Text(x, _)) => x.ends_with([' ', '\n']),
            Some(_) => false,
            None => true,
        });
        for c in t.chars() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{000C}') {
                if !prev_space {
                    s.push(' ');
                    prev_space = true;
                }
            } else {
                s.push(c);
                prev_space = false;
            }
        }
        if s.is_empty() {
            return;
        }
        self.start_para().push_text(&s, &f);
    }

    fn pre_fresh(&self) -> bool {
        self.stack.iter().rev().find(|e| e.kind == ElKind::Pre).is_some_and(|e| !e.used)
    }

    fn mark_pre_used(&mut self) {
        if let Some(e) = self.stack.iter_mut().rev().find(|e| e.kind == ElKind::Pre) {
            e.used = true;
        }
    }

    /// Pop elements down to and including index `k`.
    fn pop_to(&mut self, k: usize) {
        while self.stack.len() > k {
            let Some(e) = self.stack.pop() else { break };
            self.close(&e);
        }
    }

    fn close(&mut self, e: &El) {
        if e.kind != ElKind::Inline {
            self.flush();
        }
        match e.kind {
            ElKind::Cell => {
                self.flush();
                let blocks = if self.containers.len() > 1 { self.containers.pop().unwrap_or_default() } else { Vec::new() };
                let mut cell = self.cells.pop().unwrap_or_default();
                cell.blocks = blocks;
                if let Some(t) = self.tables.last_mut() {
                    t.row.get_or_insert_with(Vec::new).push(cell);
                }
            }
            ElKind::Tr => {
                if let Some(t) = self.tables.last_mut()
                    && let Some(r) = t.row.take()
                {
                    t.rows.push(r);
                }
            }
            ElKind::Table => {
                if let Some(mut t) = self.tables.pop() {
                    if let Some(r) = t.row.take() {
                        t.rows.push(r);
                    }
                    let mut ft = FTable { rows: t.rows.into_iter().filter(|r| !r.is_empty()).collect(), widths: Vec::new() };
                    if !ft.rows.is_empty() {
                        ft.insert_covered();
                        self.container().push(FBlock::Table(ft));
                    }
                }
            }
            ElKind::Head => {}
            _ => {}
        }
    }

    /// Index of the innermost open element named `name`, not looking past a table boundary
    /// unless `cross_tables`.
    fn find_open(&self, pred: impl Fn(&El) -> bool, stop: impl Fn(&El) -> bool) -> Option<usize> {
        for (i, e) in self.stack.iter().enumerate().rev() {
            if pred(e) {
                return Some(i);
            }
            if stop(e) {
                return None;
            }
        }
        None
    }

    fn start(&mut self, name: &str, attrs: &[(String, String)], self_close: bool) {
        // Head metadata.
        match name {
            "title" => {
                self.title = Some(String::new());
            }
            "meta" => {
                let n = attr(attrs, "name").unwrap_or("").to_ascii_lowercase();
                let c = attr(attrs, "content").unwrap_or("").to_string();
                match n.as_str() {
                    "author" => self.meta.author = c,
                    "description" => self.meta.description = c,
                    "keywords" => self.meta.keywords = c,
                    "subject" => self.meta.subject = c,
                    _ => {}
                }
                return;
            }
            _ => {}
        }
        let is_table_part = matches!(name, "tr" | "td" | "th");
        let in_table = self.tables.len() < model::MAX_DEPTH;
        // Implicit closes.
        if closes_p(name)
            && let Some(k) = self.find_open(|e| e.name == "p", |e| matches!(e.kind, ElKind::Cell | ElKind::Table | ElKind::Li | ElKind::Quote))
        {
            self.pop_to(k);
        }
        if matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") && self.stack.last().is_some_and(|e| matches!(e.kind, ElKind::Heading(_))) {
            let k = self.stack.len() - 1;
            self.pop_to(k);
        }
        if name == "li"
            && let Some(k) = self.find_open(|e| e.kind == ElKind::Li, |e| matches!(e.kind, ElKind::List(_) | ElKind::Table | ElKind::Cell))
        {
            self.pop_to(k);
        }
        if matches!(name, "dt" | "dd")
            && let Some(k) =
                self.find_open(|e| e.name == "dt" || e.name == "dd", |e| matches!(e.kind, ElKind::Table | ElKind::Cell) || e.name == "dl")
        {
            self.pop_to(k);
        }
        if is_table_part && !self.tables.is_empty() {
            if matches!(name, "td" | "th")
                && let Some(k) = self.find_open(|e| e.kind == ElKind::Cell, |e| matches!(e.kind, ElKind::Table | ElKind::Tr))
            {
                self.pop_to(k);
            }
            if name == "tr"
                && let Some(k) = self.find_open(|e| matches!(e.kind, ElKind::Tr | ElKind::Cell), |e| e.kind == ElKind::Table)
            {
                // Pop to the outermost of cell/row inside the current table.
                let mut kk = k;
                while kk > 0 && self.stack.get(kk - 1).is_some_and(|e| matches!(e.kind, ElKind::Tr | ElKind::Cell)) {
                    kk -= 1;
                }
                self.pop_to(kk);
            }
        }
        if name == "a"
            && !self.in_head()
            && let Some(id) = attr(attrs, "id").or_else(|| attr(attrs, "name")).filter(|s| !s.is_empty())
        {
            self.start_para().inlines.push(Inline::Anchor(id.to_string()));
        }
        // Element-specific work.
        match name {
            "br" => {
                if self.stack.iter().any(|e| e.kind == ElKind::Pre) {
                    self.start_para();
                    self.flush();
                } else {
                    let f = self.fmt();
                    self.start_para().push_text("\n", &f);
                }
                return;
            }
            "hr" => {
                self.flush();
                self.container().push(FBlock::Para(Para::new(Kind::Rule)));
                return;
            }
            "img" => {
                let alt = attr(attrs, "alt").unwrap_or("").to_string();
                let dim = |k: &str| attr(attrs, k).and_then(|v| v.trim().trim_end_matches("px").trim().parse::<f32>().ok()).map(|p| p * 0.75);
                let mut w = dim("width");
                let mut h = dim("height");
                if let Some(st) = attr(attrs, "style") {
                    for decl in st.split(';') {
                        if let Some((k, v)) = decl.split_once(':') {
                            match k.trim().to_ascii_lowercase().as_str() {
                                "width" => w = css_len_pt(v).or(w),
                                "height" => h = css_len_pt(v).or(h),
                                _ => {}
                            }
                        }
                    }
                }
                let img = attr(attrs, "src").and_then(data_uri).and_then(|d| make_img(d, w, h, &alt));
                let f = self.fmt();
                match img {
                    Some(im) => self.start_para().inlines.push(Inline::Image(im)),
                    None if !alt.is_empty() => self.start_para().push_text(&alt, &f),
                    None => {}
                }
                return;
            }
            _ => {}
        }
        if is_void(name) || self_close && block_kind(name).is_none() && !matches!(name, "a" | "span") {
            return;
        }
        if self.stack.len() >= MAX_STACK {
            return;
        }
        let mut kind = block_kind(name).unwrap_or(ElKind::Inline);
        if matches!(kind, ElKind::Table | ElKind::Tr | ElKind::Cell) && !in_table {
            kind = ElKind::Block;
        }
        if matches!(kind, ElKind::Tr | ElKind::Cell) && self.tables.is_empty() {
            kind = ElKind::Block;
        }
        if kind != ElKind::Inline {
            self.flush();
        }
        let mut fmt = self.fmt();
        let parent_bg = fmt.background;
        let mut align = None;
        let mut preserve = self.preserve();
        let mut page_break = false;
        match name {
            "b" | "strong" => fmt.bold = true,
            "i" | "em" | "cite" | "var" | "dfn" | "address" => fmt.italic = true,
            "u" | "ins" => fmt.underline = true,
            "s" | "strike" | "del" => {
                fmt.strike = true;
                fmt.double_strike = false;
            }
            "code" | "kbd" | "tt" | "samp" => fmt.code = true,
            "sup" => fmt.sup = true,
            "sub" => fmt.sub = true,
            "mark" => fmt.background = Some(wordcraft_doc::Rgb(255, 255, 0)),
            "font" => {
                if let Some(c) = attr(attrs, "color").and_then(parse_color) {
                    fmt.color = Some(c);
                }
                if let Some(face) = attr(attrs, "face") {
                    let fam = face.split(',').next().unwrap_or("").trim().to_string();
                    if model::is_mono(&fam) {
                        fmt.code = true;
                    } else if !fam.is_empty() {
                        fmt.font = Some(fam);
                    }
                }
                if let Some(sz) = attr(attrs, "size").and_then(|s| s.trim().parse::<i32>().ok()) {
                    fmt.size = Some([7.5, 10.0, 12.0, 13.5, 18.0, 24.0, 36.0].get((sz.clamp(1, 7) - 1) as usize).copied().unwrap_or(12.0));
                }
            }
            "a" => {
                if let Some(h) =
                    attr(attrs, "href").filter(|h| !h.trim().is_empty() && !h.trim_start().to_ascii_lowercase().starts_with("javascript:"))
                {
                    fmt.link = Some(h.trim().to_string());
                }
            }
            _ => {}
        }
        if let Some(a) = attr(attrs, "align") {
            align = match a.to_ascii_lowercase().as_str() {
                "center" | "middle" => Some(Align::Center),
                "right" => Some(Align::Right),
                "justify" => Some(Align::Justify),
                "left" => Some(Align::Left),
                _ => None,
            };
        }
        if name == "center" {
            align = Some(Align::Center);
        }
        if let Some(st) = attr(attrs, "style") {
            apply_style(st, &mut fmt, &mut align, &mut preserve, &mut page_break, matches!(name, "s" | "strike" | "del"));
        }
        if kind == ElKind::Pre {
            preserve = true;
        }
        // Paragraph formatting on block elements (not the page itself: its background stays
        // behind the text as before).
        let mut block = BlockCss::default();
        if matches!(kind, ElKind::Block | ElKind::Heading(_) | ElKind::Quote | ElKind::Pre | ElKind::Li) && !matches!(name, "body" | "html") {
            if let Some(st) = attr(attrs, "style") {
                block = block_css(st);
            }
            if block.shading.is_some() {
                fmt.background = parent_bg;
            }
            // The line between paragraphs of a bordered box WordCraft wrote.
            if let (Some(bs), Some(between)) = (block.borders.as_mut(), attr(attrs, "data-mc-between").and_then(css_border)) {
                bs.between = Some(between);
            }
        }
        let list_fmt = match kind {
            ElKind::List(_) => attr(attrs, "data-mc-list").and_then(parse_list_levels).map(|levels| ListFmt {
                levels,
                label: String::new(),
                value: attr(attrs, "start").and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(1).clamp(1, 32_767),
            }),
            _ => None,
        };
        let skip = attr(attrs, "data-mc-label").is_some();
        match kind {
            ElKind::Table => self.tables.push(TableB { rows: Vec::new(), row: None }),
            ElKind::Tr => {
                if let Some(t) = self.tables.last_mut()
                    && let Some(r) = t.row.take()
                {
                    t.rows.push(r);
                }
            }
            ElKind::Cell => {
                let tb = self.find_open(|e| e.kind == ElKind::Tr, |e| e.kind == ElKind::Table).is_none();
                if tb && let Some(t) = self.tables.last_mut() {
                    // A cell without a row: start one.
                    if let Some(r) = t.row.take() {
                        t.rows.push(r);
                    }
                    t.row = Some(Vec::new());
                }
                let num = |k: &str| attr(attrs, k).and_then(|v| v.trim().parse::<u32>().ok()).map(|v| v.clamp(1, 63));
                let shading = attr(attrs, "bgcolor").and_then(parse_color).or(fmt.background);
                self.cells.push(Cell {
                    blocks: Vec::new(),
                    colspan: num("colspan").unwrap_or(1),
                    rowspan: attr(attrs, "rowspan").and_then(|v| v.trim().parse::<u32>().ok()).map(|v| v.clamp(1, 1000)).unwrap_or(1),
                    covered: false,
                    header: name == "th",
                    shading,
                });
                fmt.background = None;
                self.containers.push(Vec::new());
            }
            _ => {}
        }
        self.stack.push(El { name: name.to_string(), kind, fmt, align, preserve, page_break, used: false, block, list_fmt, skip });
    }

    fn end(&mut self, name: &str) {
        if name == "title" {
            if let Some(t) = self.title.take() {
                self.meta.title = t.split_whitespace().collect::<Vec<_>>().join(" ");
            }
            return;
        }
        let crosses = matches!(name, "table");
        let target = if matches!(name, "td" | "th") {
            self.find_open(|e| e.kind == ElKind::Cell, |e| e.kind == ElKind::Table)
        } else if name == "tr" {
            self.find_open(|e| e.kind == ElKind::Tr, |e| e.kind == ElKind::Table)
        } else {
            self.find_open(|e| e.name == name, |e| !crosses && matches!(e.kind, ElKind::Table | ElKind::Cell))
        };
        if let Some(k) = target {
            self.pop_to(k);
        }
    }

    fn finish(mut self) -> Flow {
        self.pop_to(0);
        self.flush();
        // Unclosed cells: fold remaining containers into the body.
        while self.containers.len() > 1 {
            let c = self.containers.pop().unwrap_or_default();
            self.container().extend(c);
        }
        let blocks = self.containers.pop().unwrap_or_default();
        if let Some(t) = self.title.take() {
            self.meta.title = t;
        }
        Flow { blocks, meta: self.meta }
    }
}

/// Parse HTML text into the flow model.
pub fn parse(s: &str) -> Flow {
    let mut lx = Lexer { s, pos: 0 };
    let mut b = Builder::new();
    let mut guard = 0usize;
    while let Some(t) = lx.next() {
        guard += 1;
        if guard > 50_000_000 {
            break;
        }
        match t {
            Tok::Text(t) => b.text(&t),
            Tok::Start { name, attrs, self_close } => {
                if matches!(name.as_str(), "script" | "style" | "noscript" | "template" | "textarea") && !self_close {
                    lx.skip_past(&format!("</{name}"));
                    lx.skip_past(">");
                    continue;
                }
                b.start(&name, &attrs, self_close);
            }
            Tok::End(name) => b.end(&name),
        }
    }
    b.finish()
}

/// Parse HTML into a document.
pub fn import(s: &str) -> Document {
    model::to_doc(&parse(s))
}

// ---------------------------------------------------------------------------------------------
// Export

/// Escape text for HTML content and attribute values.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\u{000C}' | '\u{000E}' => {}
            _ => o.push(c),
        }
    }
    o
}

fn align_css(a: Option<Align>) -> Option<&'static str> {
    match a? {
        Align::Center => Some("text-align:center"),
        Align::Right => Some("text-align:right"),
        Align::Justify | Align::Distribute => Some("text-align:justify"),
        Align::Left => None,
    }
}

fn text_html(t: &str) -> String {
    let mut o = String::new();
    let mut prev_space = false;
    for c in t.chars() {
        match c {
            '\n' => {
                o.push_str("<br>");
                prev_space = true;
                continue;
            }
            '\t' => o.push_str("<span style=\"white-space:pre\">\t</span>"),
            ' ' if prev_space => o.push_str("&nbsp;"),
            _ => o.push_str(&esc(&c.to_string())),
        }
        prev_space = c == ' ';
    }
    o
}

/// Inline spans → HTML.
pub fn inlines_html(inlines: &[Inline]) -> String {
    let mut o = String::new();
    for i in inlines {
        match i {
            Inline::Text(t, f) => {
                let mut open = String::new();
                let mut close: Vec<&str> = Vec::new();
                if let Some(l) = &f.link {
                    open.push_str(&format!("<a href=\"{}\">", esc(l)));
                    close.push("</a>");
                }
                let mut tag = |o: &str, c: &'static str| {
                    open.push_str(o);
                    close.push(c);
                };
                if f.bold {
                    tag("<strong>", "</strong>");
                }
                if f.italic {
                    tag("<em>", "</em>");
                }
                if f.underline {
                    let mut css: Vec<String> = Vec::new();
                    if let Some(u) = f.underline_style.and_then(underline_css) {
                        css.push(u.to_string());
                    }
                    if let Some(c) = f.underline_color {
                        css.push(format!("text-decoration-color:#{}", c.hex()));
                    }
                    let u = if css.is_empty() { "<u>".to_string() } else { format!("<u style=\"{}\">", css.join(";")) };
                    tag(&u, "</u>");
                }
                if f.strike && f.double_strike {
                    tag("<s style=\"text-decoration-line:line-through;text-decoration-style:double\">", "</s>");
                } else if f.strike {
                    tag("<s>", "</s>");
                }
                if f.sup {
                    tag("<sup>", "</sup>");
                } else if f.sub {
                    tag("<sub>", "</sub>");
                }
                if f.code {
                    tag("<code>", "</code>");
                }
                let mut css = Vec::new();
                if let Some(c) = f.color {
                    css.push(format!("color:#{}", c.hex()));
                }
                if let Some(c) = f.background {
                    css.push(format!("background-color:#{}", c.hex()));
                }
                if let Some(s) = f.size {
                    css.push(format!("font-size:{}pt", trim_num(s)));
                }
                if let Some(fam) = &f.font {
                    css.push(format!("font-family:{}", font_stack(fam)));
                }
                if f.caps {
                    css.push("text-transform:uppercase".into());
                }
                if f.small_caps {
                    css.push("font-variant:small-caps".into());
                }
                if let Some(sp) = f.spacing.filter(|x| x.is_finite() && *x != 0.0) {
                    css.push(format!("letter-spacing:{}pt", trim_num(sp)));
                }
                if let Some(pos) = f.position.filter(|x| x.is_finite() && *x != 0.0 && !f.sup && !f.sub) {
                    css.push(format!("vertical-align:{}pt", trim_num(pos)));
                }
                let mut shadows: Vec<&str> = Vec::new();
                if f.shadow {
                    shadows.push(SHADOW_CSS);
                }
                if f.emboss {
                    shadows.push(EMBOSS_CSS);
                }
                if f.engrave {
                    shadows.push(ENGRAVE_CSS);
                }
                if !shadows.is_empty() {
                    css.push(format!("text-shadow:{}", shadows.join(",")));
                }
                if f.outline {
                    // Hollow letters where the client draws strokes; elsewhere plain text.
                    let c = f.color.unwrap_or(Rgb(0, 0, 0));
                    css.push(format!("-webkit-text-stroke:1px #{};-webkit-text-fill-color:#ffffff", c.hex()));
                }
                if !css.is_empty() {
                    open.push_str(&format!("<span style=\"{}\">", css.join(";")));
                    close.push("</span>");
                }
                o.push_str(&open);
                o.push_str(&text_html(t));
                for c in close.iter().rev() {
                    o.push_str(c);
                }
            }
            Inline::Image(img) => {
                o.push_str(&format!(
                    "<img src=\"data:{};base64,{}\" alt=\"{}\" width=\"{}\" height=\"{}\">",
                    mime_of(&img.ext),
                    base64_encode(&img.data),
                    esc(&img.alt),
                    (img.w / 0.75).round(),
                    (img.h / 0.75).round()
                ));
            }
            Inline::Anchor(a) => o.push_str(&format!("<a id=\"{}\"></a>", esc(a))),
        }
    }
    o
}

fn trim_num(v: f32) -> String {
    let s = format!("{:.2}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" { "0".into() } else { s }
}

/// CSS for an underline that is not a plain single line. Clients without decoration styles
/// (Outlook for Windows) still draw the `<u>` as a single line.
fn underline_css(u: Underline) -> Option<&'static str> {
    Some(match u {
        Underline::Double => "text-decoration-style:double",
        Underline::Thick => "text-decoration-thickness:2px",
        Underline::Dotted => "text-decoration-style:dotted",
        Underline::Dash | Underline::DotDash | Underline::DotDotDash => "text-decoration-style:dashed",
        Underline::Wave | Underline::DoubleWave => "text-decoration-style:wavy",
        Underline::None | Underline::Single | Underline::Words => return None,
    })
}

/// A font family with a generic fallback, for readers who don't have the font.
fn font_stack(fam: &str) -> String {
    let f: String = fam.chars().filter(|c| !matches!(c, '\'' | '"' | ';' | '<' | '>' | '{' | '}')).collect();
    let l = f.to_ascii_lowercase();
    const SERIF: [&str; 14] = [
        "times",
        "georgia",
        "cambria",
        "garamond",
        "palatino",
        "book antiqua",
        "baskerville",
        "constantia",
        "bodoni",
        "didot",
        "century",
        "serif",
        "minion",
        "caslon",
    ];
    let generic = if model::is_mono(&f) {
        "monospace"
    } else if !l.contains("sans") && SERIF.iter().any(|s| l.contains(s)) {
        "serif"
    } else {
        "sans-serif"
    };
    format!("'{}',{generic}", esc(&f))
}

/// One paragraph border side as CSS (with its distance from the text as padding).
fn border_css(side: &str, b: Option<Border>) -> Option<String> {
    let b = b?;
    let style = match b.style {
        BorderStyle::None => return None,
        BorderStyle::Single | BorderStyle::Thick | BorderStyle::Wave => "solid",
        BorderStyle::Double | BorderStyle::Triple => "double",
        BorderStyle::Dotted => "dotted",
        BorderStyle::Dashed | BorderStyle::DotDash => "dashed",
    };
    if !(b.width.is_finite() && b.width > 0.0) {
        return None;
    }
    let w = match b.style {
        BorderStyle::Double | BorderStyle::Triple => (b.width * 3.0).max(2.25),
        BorderStyle::Thick => b.width.max(2.25),
        _ => b.width,
    };
    let color = b.color.map(|c| c.hex()).unwrap_or_else(|| "000000".into());
    let mut css = format!("border-{side}:{}pt {style} #{color}", trim_num(w.min(18.0)));
    if b.space.is_finite() && b.space > 0.0 {
        css.push_str(&format!(";padding-{side}:{}pt", trim_num(b.space.min(31.0))));
    }
    Some(css)
}

/// A border written as a value only (for `data-mc-between`).
fn border_value(b: Option<Border>) -> Option<String> {
    border_css("top", b).and_then(|c| c.split(';').next().and_then(|x| x.strip_prefix("border-top:")).map(str::to_string))
}

/// Line spacing as CSS, the way Word writes it for mail (single = `normal`).
fn line_css(l: LineSpacing) -> Option<String> {
    match l {
        LineSpacing::Multiple(m) if m.is_finite() && (m - 1.0).abs() < 0.005 => Some("line-height:normal".into()),
        LineSpacing::Multiple(m) if m.is_finite() && m > 0.0 => Some(format!("line-height:{}%", (m * 100.0).round())),
        LineSpacing::Exactly(x) if x.is_finite() && x > 0.0 => Some(format!("line-height:{}pt;mso-line-height-rule:exactly", trim_num(x))),
        LineSpacing::AtLeast(x) if x.is_finite() && x > 0.0 => Some(format!("line-height:{}pt", trim_num(x))),
        _ => None,
    }
}

/// A paragraph's formatting as inline CSS. `list`: a list item (its nesting indents it);
/// `borders`: write the paragraph's own borders (off inside a bordered box).
fn para_css(p: &Para, list: bool, borders: bool) -> Vec<String> {
    let mut css: Vec<String> = Vec::new();
    if let Some(a) = align_css(p.align) {
        css.push(a.into());
    }
    if p.page_break {
        css.push("break-before:page;page-break-before:always".into());
    }
    let pt = |v: Option<f32>| v.filter(|x| x.is_finite()).map(|x| trim_num(x.clamp(-1584.0, 1584.0)));
    if !list {
        if let Some(x) = pt(p.indent_left) {
            css.push(format!("margin-left:{x}pt"));
        }
        if let Some(x) = pt(p.indent_first) {
            css.push(format!("text-indent:{x}pt"));
        }
    }
    if let Some(x) = pt(p.indent_right) {
        css.push(format!("margin-right:{x}pt"));
    }
    if let Some(x) = pt(p.space_before.map(|v| v.max(0.0))) {
        css.push(format!("margin-top:{x}pt"));
    }
    if let Some(x) = pt(p.space_after.map(|v| v.max(0.0))) {
        css.push(format!("margin-bottom:{x}pt"));
    }
    if let Some(l) = p.line_spacing.and_then(line_css) {
        css.push(l);
    }
    if let Some(c) = p.shading {
        css.push(format!("background-color:#{}", c.hex()));
    }
    if borders && let Some(b) = p.borders {
        for (side, x) in [("top", b.top), ("right", b.right), ("bottom", b.bottom), ("left", b.left)] {
            if let Some(c) = border_css(side, x) {
                css.push(c);
            }
        }
    }
    css
}

fn style_attr(css: &[String]) -> String {
    if css.is_empty() { String::new() } else { format!(" style=\"{}\"", css.join(";")) }
}

fn para_attrs(p: &Para) -> String {
    style_attr(&para_css(p, false, true))
}

fn para_html(p: &Para) -> String {
    para_html_with(p, &para_attrs(p))
}

fn para_html_with(p: &Para, attrs: &str) -> String {
    let body = inlines_html(&p.inlines);
    match p.kind {
        Kind::Heading(n) => format!("<h{n}{attrs}>{body}</h{n}>", n = n.clamp(1, 6)),
        Kind::Title => format!("<h1{attrs}>{body}</h1>"),
        Kind::Rule => "<hr>".to_string(),
        _ if body.is_empty() => format!("<p{attrs}>&nbsp;</p>"),
        _ => format!("<p{attrs}>{body}</p>"),
    }
}

fn table_html(t: &FTable, out: &mut String, depth: usize) {
    out.push_str("<table style=\"border-collapse:collapse\">\n");
    for row in &t.rows {
        out.push_str("<tr>");
        for c in row {
            if c.covered {
                continue;
            }
            let tag = if c.header { "th" } else { "td" };
            let mut attrs = String::new();
            if c.colspan > 1 {
                attrs.push_str(&format!(" colspan=\"{}\"", c.colspan));
            }
            if c.rowspan > 1 {
                attrs.push_str(&format!(" rowspan=\"{}\"", c.rowspan));
            }
            let mut css = vec!["border:1px solid #999".to_string(), "padding:4px 6px".to_string(), "vertical-align:top".to_string()];
            if let Some(s) = c.shading {
                css.push(format!("background-color:#{}", s.hex()));
            }
            let single = match c.blocks.as_slice() {
                [FBlock::Para(p)] if p.kind == Kind::Normal && p.list.is_none() && !p.page_break => Some(p),
                _ => None,
            };
            if let Some(a) = single.and_then(|p| align_css(p.align)) {
                css.push(a.to_string());
            }
            out.push_str(&format!("<{tag}{attrs} style=\"{}\">", css.join(";")));
            match single {
                Some(p) => out.push_str(&inlines_html(&p.inlines)),
                None => {
                    out.push('\n');
                    blocks_html(&c.blocks, out, depth + 1);
                }
            }
            out.push_str(&format!("</{tag}>"));
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table>\n");
}

fn blocks_html(blocks: &[FBlock], out: &mut String, depth: usize) {
    let mut i = 0;
    while let Some(b) = blocks.get(i) {
        match b {
            FBlock::Table(t) => {
                if depth < model::MAX_DEPTH {
                    table_html(t, out, depth);
                }
                i += 1;
            }
            FBlock::Para(p) if p.list.is_some() => {
                // Nested lists from consecutive list paragraphs.
                let mut stack: Vec<(ListMark, bool)> = Vec::new(); // (list, li open)
                while let Some(FBlock::Para(q)) = blocks.get(i) {
                    let Some(li) = q.list else { break };
                    let lv = li.level.min(8) as usize;
                    let mark = list_mark(q.list_fmt.as_ref(), li.level, li.ordered);
                    while stack.len() > lv + 1 {
                        close_list(&mut stack, out);
                    }
                    if stack.len() == lv + 1 && stack.last().is_some_and(|s| s.0.key != mark.key) {
                        close_list(&mut stack, out);
                    }
                    while stack.len() < lv + 1 {
                        if let Some(top) = stack.last_mut()
                            && !top.1
                        {
                            out.push_str("<li style=\"list-style-type:none\">");
                            top.1 = true;
                        }
                        let d = stack.len();
                        let m = if d == lv { mark.clone() } else { list_mark(q.list_fmt.as_ref(), d as u8, li.ordered) };
                        let start = q.list_fmt.as_ref().map(|f| f.value).filter(|v| d == lv && m.tag == "ol" && *v > 1);
                        out.push_str(&m.open(start, d == 0));
                        stack.push((m, false));
                    }
                    if let Some(top) = stack.last_mut() {
                        if top.1 {
                            out.push_str("</li>\n");
                        }
                        top.1 = true;
                    }
                    let mut body = inlines_html(&q.inlines);
                    if mark.literal {
                        // A marker list styles can't draw (1), 1.1., ✓): written out, and
                        // marked so WordCraft reads it back as the list's own number.
                        let label = q.list_fmt.as_ref().map(|f| f.label.as_str()).unwrap_or("");
                        if !label.is_empty() {
                            body = format!("<span data-mc-label=\"1\">{}&nbsp;</span>{body}", esc(label));
                        }
                    }
                    let attrs = style_attr(&para_css(q, true, true));
                    out.push_str(&format!("<li{attrs}>{body}"));
                    i += 1;
                }
                while !stack.is_empty() {
                    close_list(&mut stack, out);
                }
                out.push('\n');
            }
            FBlock::Para(p) if p.kind == Kind::Code => {
                out.push_str("<pre><code>");
                let mut first = true;
                while let Some(FBlock::Para(q)) = blocks.get(i) {
                    if q.kind != Kind::Code || q.list.is_some() {
                        break;
                    }
                    if !first {
                        out.push('\n');
                    }
                    first = false;
                    out.push_str(&esc(&q.text()));
                    i += 1;
                }
                out.push_str("</code></pre>\n");
            }
            FBlock::Para(p) if p.kind == Kind::Quote => {
                out.push_str("<blockquote>\n");
                while let Some(FBlock::Para(q)) = blocks.get(i) {
                    if q.kind != Kind::Quote || q.list.is_some() {
                        break;
                    }
                    out.push_str(&para_html(&Para { kind: Kind::Normal, ..q.clone() }));
                    out.push('\n');
                    i += 1;
                }
                out.push_str("</blockquote>\n");
            }
            FBlock::Para(p)
                if boxable(p)
                    && p.borders.is_some()
                    && matches!(blocks.get(i + 1), Some(FBlock::Para(q)) if boxable(q) && q.borders == p.borders) =>
            {
                // Paragraphs sharing borders are one box, as Word draws them (and writes them).
                let b = p.borders.unwrap_or_default();
                let mut j = i;
                while matches!(blocks.get(j), Some(FBlock::Para(q)) if boxable(q) && q.borders == p.borders) {
                    j += 1;
                }
                let group: Vec<&Para> =
                    blocks.get(i..j).unwrap_or(&[]).iter().filter_map(|x| if let FBlock::Para(q) = x { Some(q) } else { None }).collect();
                let mut css: Vec<String> = Vec::new();
                for (side, x) in [("top", b.top), ("right", b.right), ("bottom", b.bottom), ("left", b.left)] {
                    if let Some(c) = border_css(side, x) {
                        css.push(c);
                    }
                }
                if let Some(c) = p.shading.filter(|_| group.iter().all(|q| q.shading == p.shading)) {
                    css.push(format!("background-color:#{}", c.hex()));
                }
                let between = border_value(b.between).map(|v| format!(" data-mc-between=\"{}\"", esc(&v))).unwrap_or_default();
                out.push_str(&format!("<div{}{between}>\n", style_attr(&css)));
                for (k, q) in group.iter().enumerate() {
                    let mut pc = para_css(q, false, false);
                    if k > 0
                        && let Some(c) = border_css("top", b.between)
                    {
                        pc.push(c);
                    }
                    out.push_str(&para_html_with(q, &style_attr(&pc)));
                    out.push('\n');
                }
                out.push_str("</div>\n");
                i = j.max(i + 1);
            }
            FBlock::Para(p) => {
                out.push_str(&para_html(p));
                out.push('\n');
                i += 1;
            }
        }
    }
}

/// A plain paragraph (no list, quote or code) that can sit in a bordered box.
fn boxable(p: &Para) -> bool {
    p.list.is_none() && matches!(p.kind, Kind::Normal | Kind::Heading(_) | Kind::Title)
}

/// How a list level is written: a list style every mail reader knows (disc, decimal, lower
/// alpha…), or, for markers no list style draws, `list-style-type:none` with the marker in the
/// text.
#[derive(Clone, Debug, PartialEq)]
struct ListMark {
    tag: &'static str,
    /// `type` attribute and CSS list style (none for a plain `<ol>`/`<ul>`).
    style: Option<(&'static str, &'static str)>,
    literal: bool,
    /// The list's levels, so WordCraft can read the numbering back (`data-mc-list`).
    data: Option<String>,
    /// Lists with different keys can't share a container.
    key: String,
}

impl ListMark {
    /// The start tag; `outer`: the outermost list of a nest, which carries the numbering.
    fn open(&self, start: Option<u32>, outer: bool) -> String {
        let mut s = format!("<{}", self.tag);
        if let Some((ty, css)) = self.style {
            if !ty.is_empty() {
                s.push_str(&format!(" type=\"{ty}\""));
            }
            if let Some(n) = start {
                s.push_str(&format!(" start=\"{n}\""));
            }
            s.push_str(&format!(" style=\"list-style-type:{css}\""));
        } else if let Some(n) = start {
            s.push_str(&format!(" start=\"{n}\""));
        }
        if let Some(d) = self.data.as_ref().filter(|_| outer) {
            s.push_str(&format!(" data-mc-list=\"{}\"", esc(d)));
        }
        s.push('>');
        s
    }
}

fn list_mark(fmt: Option<&ListFmt>, level: u8, ordered: bool) -> ListMark {
    let tag = if ordered { "ol" } else { "ul" };
    let Some(f) = fmt else {
        return ListMark { tag, style: None, literal: false, data: None, key: tag.into() };
    };
    let data = (!f.levels.iter().any(|(_, t)| t.contains('|')))
        .then(|| f.levels.iter().map(|(n, t)| format!("{}:{t}", n.ooxml())).collect::<Vec<_>>().join("|"));
    let native: Option<(&'static str, &'static str)> = match f.level(level) {
        Some((NumFormat::Bullet, t)) => match t.as_str() {
            "•" | "" => Some(("disc", "disc")),
            "○" | "◦" | "o" => Some(("circle", "circle")),
            "▪" | "■" => Some(("square", "square")),
            _ => None,
        },
        Some((n, t)) if *t == format!("%{}.", level as u32 + 1) => match n {
            NumFormat::Decimal => Some(("1", "decimal")),
            NumFormat::LowerLetter => Some(("a", "lower-alpha")),
            NumFormat::UpperLetter => Some(("A", "upper-alpha")),
            NumFormat::LowerRoman => Some(("i", "lower-roman")),
            NumFormat::UpperRoman => Some(("I", "upper-roman")),
            _ => None,
        },
        _ => None,
    };
    // `type` on <ul> is obsolete; the CSS carries bullets.
    let style = match native {
        Some((_, css)) if tag == "ul" => Some(("", css)),
        Some(x) => Some(x),
        None => Some(("", "none")),
    };
    let literal = native.is_none();
    let key = format!("{tag}:{style:?}:{}", data.as_deref().unwrap_or(""));
    ListMark { tag, style, literal, data, key }
}

fn close_list(stack: &mut Vec<(ListMark, bool)>, out: &mut String) {
    if let Some((m, li_open)) = stack.pop() {
        if li_open {
            out.push_str("</li>");
        }
        out.push_str(if m.tag == "ol" { "</ol>" } else { "</ul>" });
    }
}

/// A complete HTML document.
pub fn export(doc: &Document) -> String {
    let flow = model::from_doc(doc);
    export_flow(&flow, doc.styles.default_chr.lang.as_deref().unwrap_or("en"))
}

pub fn export_flow(flow: &Flow, lang: &str) -> String {
    let m = &flow.meta;
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n");
    out.push_str(&format!("<html lang=\"{}\">\n<head>\n<meta charset=\"utf-8\">\n", esc(lang)));
    out.push_str("<meta name=\"generator\" content=\"WordCraft\">\n");
    out.push_str(&format!("<title>{}</title>\n", esc(&m.title)));
    for (k, v) in [("author", &m.author), ("description", &m.description), ("keywords", &m.keywords), ("subject", &m.subject)] {
        if !v.is_empty() {
            out.push_str(&format!("<meta name=\"{k}\" content=\"{}\">\n", esc(v)));
        }
    }
    out.push_str(
        "<style>\nbody{font-family:Aptos,Calibri,'Segoe UI',Arial,sans-serif;font-size:12pt;line-height:1.15;max-width:48em;margin:2em auto;padding:0 1em}\n\
         table{border-collapse:collapse;margin:0.5em 0}\nblockquote{margin:0.5em 2em;font-style:italic;color:#404040}\n\
         pre{background:#f5f5f5;padding:0.5em;font-family:'Courier New',monospace;font-size:10pt}\n</style>\n</head>\n<body>\n",
    );
    blocks_html(&flow.blocks, &mut out, 0);
    out.push_str("</body>\n</html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(f: &Flow) -> Vec<String> {
        f.blocks
            .iter()
            .map(|b| match b {
                FBlock::Para(p) => format!("{:?}{:?}:{}", p.kind, p.list.map(|l| (l.ordered, l.level)), p.text()),
                FBlock::Table(t) => format!("table{}x{}", t.rows.len(), t.cols()),
            })
            .collect()
    }

    #[test]
    fn tag_soup() {
        let f = parse(
            "<html><head><title>T &amp; U</title><style>p{}</style><script>if(a<b)x</script></head><body><h1>Head</h1><p>One <b>bold<p>Two <i>it</i>\n  text<ul><li>a<li>b<ol><li>c</ol></ul><table><tr><td>x<td colspan=2>y<tr><th>z</table><blockquote>q</blockquote><pre>l1\nl2</pre>",
        );
        assert_eq!(f.meta.title, "T & U");
        assert_eq!(
            texts(&f),
            vec![
                "Heading(1)None:Head",
                "NormalNone:One bold",
                "NormalNone:Two it text",
                "NormalSome((false, 0)):a",
                "NormalSome((false, 0)):b",
                "NormalSome((true, 1)):c",
                "table2x3",
                "QuoteNone:q",
                "CodeNone:l1",
                "CodeNone:l2",
            ]
        );
        let FBlock::Para(p) = &f.blocks[1] else { panic!() };
        assert!(matches!(&p.inlines[1], Inline::Text(t, f) if t == "bold" && f.bold));
    }

    #[test]
    fn styles_and_links() {
        let f = parse("<p style='text-align:center'><span style=\"color:#ff0000;font-weight:700\">red</span> <a href=\"http://x\">l</a><br>n</p>");
        let FBlock::Para(p) = &f.blocks[0] else { panic!() };
        assert_eq!(p.align, Some(Align::Center));
        assert!(matches!(&p.inlines[0], Inline::Text(t, f) if t == "red" && f.bold && f.color == Some(wordcraft_doc::Rgb(255,0,0))));
        assert!(p.inlines.iter().any(|i| matches!(i, Inline::Text(t, f) if t == "l" && f.link.as_deref() == Some("http://x"))));
        assert!(p.text().ends_with("l\nn"));
    }

    #[test]
    fn entities() {
        assert_eq!(unescape("a &lt;b&gt; &#65;&#x42; &bogus; &"), "a <b> AB &bogus; &");
    }

    // -----------------------------------------------------------------------------------------
    // Mail formatting: everything the Font and Paragraph groups set must reach the HTML as
    // inline CSS / tags mail readers draw, and come back when WordCraft reads its own HTML
    // (drafts, replies).

    use wordcraft_doc::numbering::ListKind;
    use wordcraft_doc::props::{Highlight, NumRef, TextColor, VertAlign};
    use wordcraft_doc::{CharProps, Paragraph, para_block};

    fn doc_of(paras: Vec<Paragraph>) -> Document {
        let mut d = Document::new();
        d.body = paras.into_iter().map(para_block).collect();
        d
    }

    fn run(text: &str, cp: CharProps) -> String {
        let html = export(&doc_of(vec![Paragraph::with_text(text, cp)]));
        let body = html.split("<body>").nth(1).unwrap_or("").to_string();
        body.split("</body>").next().unwrap_or("").trim().to_string()
    }

    fn body(d: &Document) -> String {
        let html = export(d);
        html.split("<body>\n").nth(1).unwrap_or("").split("</body>").next().unwrap_or("").to_string()
    }

    /// The first text span of the first paragraph after export and import again.
    fn back(html: &str) -> Fmt {
        let f = parse(html);
        let FBlock::Para(p) = &f.blocks[0] else { panic!("no paragraph") };
        match &p.inlines[0] {
            Inline::Text(_, f) => f.clone(),
            other => panic!("{other:?}"),
        }
    }

    fn first_para(html: &str) -> Para {
        let f = parse(html);
        let FBlock::Para(p) = &f.blocks[0] else { panic!("no paragraph") };
        p.clone()
    }

    #[test]
    fn mail_font_family_size_and_colour() {
        let h = run(
            "x",
            CharProps { font: Some("Georgia".into()), size: Some(14.0), color: Some(TextColor::Rgb(Rgb(0xC0, 0, 0))), ..Default::default() },
        );
        assert!(h.contains("font-family:'Georgia',serif"), "{h}");
        assert!(h.contains("font-size:14pt"), "{h}");
        assert!(h.contains("color:#C00000"), "{h}");
        let h2 = run("x", CharProps { font: Some("Aptos".into()), ..Default::default() });
        assert!(h2.contains("font-family:'Aptos',sans-serif"), "{h2}");
        let f = back(&h);
        assert_eq!((f.font.as_deref(), f.size, f.color), (Some("Georgia"), Some(14.0), Some(Rgb(0xC0, 0, 0))));
    }

    #[test]
    fn mail_highlight_and_character_shading() {
        let h = run("x", CharProps { highlight: Some(Highlight::Yellow), ..Default::default() });
        assert!(h.contains("background-color:#FFFF00"), "{h}");
        assert_eq!(back(&h).background, Some(Rgb(255, 255, 0)));
    }

    #[test]
    fn mail_bold_italic_strike_sub_sup() {
        let h = run("x", CharProps { bold: Some(true), italic: Some(true), strike: Some(true), ..Default::default() });
        assert!(h.contains("<strong><em><s>x</s></em></strong>"), "{h}");
        assert!(run("x", CharProps { vert_align: Some(VertAlign::Subscript), ..Default::default() }).contains("<sub>x</sub>"));
        assert!(run("x", CharProps { vert_align: Some(VertAlign::Superscript), ..Default::default() }).contains("<sup>x</sup>"));
        let f = back(&h);
        assert!(f.bold && f.italic && f.strike && !f.double_strike);
    }

    #[test]
    fn mail_underline_styles() {
        let u = |s: Underline| run("x", CharProps { underline: Some(s), ..Default::default() });
        assert!(u(Underline::Single).contains("<u>x</u>"));
        for (style, css, back_as) in [
            (Underline::Double, "text-decoration-style:double", Underline::Double),
            (Underline::Dotted, "text-decoration-style:dotted", Underline::Dotted),
            (Underline::Dash, "text-decoration-style:dashed", Underline::Dash),
            (Underline::DotDash, "text-decoration-style:dashed", Underline::Dash),
            (Underline::Wave, "text-decoration-style:wavy", Underline::Wave),
            (Underline::Thick, "text-decoration-thickness:2px", Underline::Thick),
        ] {
            let h = u(style);
            assert!(h.contains(&format!("<u style=\"{css}\">x</u>")), "{style:?}: {h}");
            let f = back(&h);
            assert!(f.underline, "{style:?}");
            assert_eq!(f.underline_style, Some(back_as), "{style:?}");
        }
        // Words-only has no CSS: a plain underline.
        assert!(u(Underline::Words).contains("<u>x</u>"));
        let h = run("x", CharProps { underline: Some(Underline::Single), underline_color: Some(Rgb(0, 0x70, 0xC0)), ..Default::default() });
        assert!(h.contains("text-decoration-color:#0070C0"), "{h}");
        assert_eq!(back(&h).underline_color, Some(Rgb(0, 0x70, 0xC0)));
    }

    #[test]
    fn mail_double_strikethrough() {
        let h = run("x", CharProps { double_strike: Some(true), ..Default::default() });
        assert!(h.contains("<s style=\"text-decoration-line:line-through;text-decoration-style:double\">x</s>"), "{h}");
        let f = back(&h);
        assert!(f.strike && f.double_strike && !f.underline && f.underline_style.is_none());
        // And into the document as double strikethrough again.
        let cp = model::char_props(&f);
        assert_eq!((cp.strike, cp.double_strike), (None, Some(true)));
    }

    #[test]
    fn mail_caps_spacing_and_position() {
        let h = run("x", CharProps { caps: Some(true), small_caps: Some(true), spacing: Some(1.5), position: Some(3.0), ..Default::default() });
        for css in ["text-transform:uppercase", "font-variant:small-caps", "letter-spacing:1.5pt", "vertical-align:3pt"] {
            assert!(h.contains(css), "{css}: {h}");
        }
        let f = back(&h);
        assert!(f.caps && f.small_caps);
        assert_eq!((f.spacing, f.position), (Some(1.5), Some(3.0)));
    }

    #[test]
    fn mail_text_effects() {
        let h = run("x", CharProps { shadow: Some(true), ..Default::default() });
        assert!(h.contains(&format!("text-shadow:{SHADOW_CSS}")), "{h}");
        let f = back(&h);
        assert!(f.shadow && !f.outline && !f.emboss);
        let h = run("x", CharProps { outline: Some(true), color: Some(TextColor::Rgb(Rgb(0, 0x70, 0xC0))), ..Default::default() });
        assert!(h.contains("-webkit-text-stroke:1px #0070C0;-webkit-text-fill-color:#ffffff"), "{h}");
        assert!(back(&h).outline);
        let h = run("x", CharProps { emboss: Some(true), ..Default::default() });
        let f = back(&h);
        assert!(f.emboss && !f.shadow && !f.engrave, "{h}");
        let h = run("x", CharProps { engrave: Some(true), ..Default::default() });
        let f = back(&h);
        assert!(f.engrave && !f.shadow && !f.emboss, "{h}");
    }

    #[test]
    fn mail_alignment_indents_and_spacing() {
        let mut p = Paragraph::with_text("para", CharProps::default());
        p.props.align = Some(Align::Justify);
        p.props.indent_left = Some(36.0);
        p.props.indent_right = Some(18.0);
        p.props.indent_first = Some(-18.0);
        p.props.space_before = Some(12.0);
        p.props.space_after = Some(0.0);
        p.props.line_spacing = Some(LineSpacing::Multiple(1.5));
        let h = body(&doc_of(vec![p]));
        for css in [
            "text-align:justify",
            "margin-left:36pt",
            "text-indent:-18pt",
            "margin-right:18pt",
            "margin-top:12pt",
            "margin-bottom:0pt",
            "line-height:150%",
        ] {
            assert!(h.contains(css), "{css}: {h}");
        }
        let q = first_para(&h);
        assert_eq!(q.align, Some(Align::Justify));
        assert_eq!((q.indent_left, q.indent_right, q.indent_first), (Some(36.0), Some(18.0), Some(-18.0)));
        assert_eq!((q.space_before, q.space_after), (Some(12.0), Some(0.0)));
        assert_eq!(q.line_spacing, Some(LineSpacing::Multiple(1.5)));
    }

    #[test]
    fn mail_line_spacing_rules() {
        let ls = |l: LineSpacing| {
            let mut p = Paragraph::with_text("x", CharProps::default());
            p.props.line_spacing = Some(l);
            body(&doc_of(vec![p]))
        };
        assert!(ls(LineSpacing::Multiple(1.0)).contains("line-height:normal"));
        assert!(ls(LineSpacing::Multiple(2.0)).contains("line-height:200%"));
        let h = ls(LineSpacing::Exactly(18.0));
        assert!(h.contains("line-height:18pt;mso-line-height-rule:exactly"), "{h}");
        assert_eq!(first_para(&h).line_spacing, Some(LineSpacing::Exactly(18.0)));
        let h = ls(LineSpacing::AtLeast(14.0));
        assert_eq!(first_para(&h).line_spacing, Some(LineSpacing::AtLeast(14.0)));
    }

    #[test]
    fn mail_paragraph_shading_and_borders() {
        let mut p = Paragraph::with_text("boxed", CharProps::default());
        p.props.shading = Some(Rgb(0xDE, 0xEA, 0xF6));
        let line = Border { style: BorderStyle::Single, width: 0.5, color: None, space: 1.0 };
        p.props.borders = Some(Borders { bottom: Some(line), ..Default::default() });
        let h = body(&doc_of(vec![p]));
        assert!(h.contains("background-color:#DEEAF6"), "{h}");
        assert!(h.contains("border-bottom:0.5pt solid #000000;padding-bottom:1pt"), "{h}");
        let q = first_para(&h);
        assert_eq!(q.shading, Some(Rgb(0xDE, 0xEA, 0xF6)));
        let b = q.borders.and_then(|b| b.bottom).unwrap();
        assert_eq!((b.style, b.width, b.space), (BorderStyle::Single, 0.5, 1.0));
        // The shading is the paragraph's, not the text's.
        assert!(matches!(&q.inlines[0], Inline::Text(_, f) if f.background.is_none()));
    }

    #[test]
    fn mail_bordered_paragraphs_share_one_box() {
        let line = Border { style: BorderStyle::Single, width: 0.5, color: Some(Rgb(0xC0, 0, 0)), space: 1.0 };
        let all = Borders::all(line);
        let mut ps = Vec::new();
        for t in ["one", "two", "three"] {
            let mut p = Paragraph::with_text(t, CharProps::default());
            p.props.borders = Some(all);
            ps.push(p);
        }
        ps.push(Paragraph::with_text("after", CharProps::default()));
        let h = body(&doc_of(ps));
        assert_eq!(h.matches("<div").count(), 1, "{h}");
        assert!(h.starts_with("<div style=\"border-top:0.5pt solid #C00000"), "{h}");
        assert!(h.contains("data-mc-between=\"0.5pt solid #C00000\""), "{h}");
        // The line between: on the second and third paragraphs only.
        assert_eq!(h.matches("<p style=\"border-top").count(), 2, "{h}");
        assert!(h.contains("</div>\n<p>after</p>"), "{h}");
        let f = parse(&h);
        let borders: Vec<Option<Borders>> = f.blocks.iter().map(|b| if let FBlock::Para(p) = b { p.borders } else { None }).collect();
        assert_eq!(borders[0], borders[1]);
        assert_eq!(borders[1], borders[2]);
        assert!(borders[0].and_then(|b| b.between).is_some());
        assert_eq!(borders[3], None);
        // Back in a document, the three are one bordered group again.
        let d = model::to_doc(&f);
        let p0 = d.body[0].as_para().unwrap();
        assert_eq!(p0.props.borders.and_then(|b| b.left).map(|b| b.color), Some(Some(Rgb(0xC0, 0, 0))));
    }

    fn list_doc(kind: ListKind, items: &[(&str, u8)]) -> Document {
        let mut d = Document::new();
        let num = d.numbering.add_list(kind);
        d.body = items
            .iter()
            .map(|(t, lv)| {
                let mut p = Paragraph::with_text(t, CharProps::default()).styled("ListParagraph");
                p.props.numbering = Some(NumRef { num, level: *lv });
                para_block(p)
            })
            .collect();
        d
    }

    #[test]
    fn mail_bullets() {
        let h = body(&list_doc(ListKind::Bullet, &[("a", 0), ("b", 1), ("c", 2)]));
        assert!(h.starts_with("<ul style=\"list-style-type:disc\""), "{h}");
        assert!(h.contains("<ul style=\"list-style-type:circle\""), "{h}");
        assert!(h.contains("<ul style=\"list-style-type:square\""), "{h}");
        assert!(!h.contains("data-mc-label"), "{h}");
        // A bullet no list style draws is written out.
        let h = body(&list_doc(ListKind::BulletChar('✓'), &[("done", 0), ("also", 0)]));
        assert!(h.contains("list-style-type:none"), "{h}");
        assert!(h.contains("<li><span data-mc-label=\"1\">✓&nbsp;</span>done"), "{h}");
        // Read back: the marker is the list's, not text.
        let f = parse(&h);
        let FBlock::Para(p) = &f.blocks[0] else { panic!() };
        assert_eq!(p.text(), "done");
        let d = model::to_doc(&f);
        let n = d.body[0].as_para().and_then(|p| p.props.numbering).unwrap();
        let lv = d.numbering.level(n.num, 0).unwrap();
        assert_eq!((lv.format, lv.text.as_str()), (NumFormat::Bullet, "✓"));
    }

    #[test]
    fn mail_numbering_kinds() {
        for (kind, want) in [
            (ListKind::Numbered, "<ol type=\"1\" style=\"list-style-type:decimal\""),
            (ListKind::UpperLetter, "<ol type=\"A\" style=\"list-style-type:upper-alpha\""),
            (ListKind::LowerRoman, "<ol type=\"i\" style=\"list-style-type:lower-roman\""),
            (ListKind::Outline, "<ol type=\"I\" style=\"list-style-type:upper-roman\""),
        ] {
            let h = body(&list_doc(kind, &[("one", 0), ("two", 0)]));
            assert!(h.starts_with(want), "{kind:?}: {h}");
            assert!(!h.contains("data-mc-label"), "{kind:?}: {h}");
        }
        // 1) a) — no list style has the bracket: written out.
        let h = body(&list_doc(ListKind::NumberedParen, &[("one", 0), ("two", 0)]));
        assert!(h.contains("<span data-mc-label=\"1\">1)&nbsp;</span>one"), "{h}");
        assert!(h.contains("<span data-mc-label=\"1\">2)&nbsp;</span>two"), "{h}");
        let h = body(&list_doc(ListKind::LowerLetter, &[("one", 0)]));
        assert!(h.contains(">a)&nbsp;</span>one"), "{h}");
        // Numbering read back as the same list kind.
        let d = model::to_doc(&parse(&body(&list_doc(ListKind::NumberedParen, &[("one", 0), ("two", 0)]))));
        let n = d.body[0].as_para().and_then(|p| p.props.numbering).unwrap();
        let lv = d.numbering.level(n.num, 0).unwrap();
        assert_eq!((lv.format, lv.text.as_str()), (NumFormat::Decimal, "%1)"));
        assert_eq!(d.body[1].as_para().and_then(|p| p.props.numbering).map(|x| x.num), Some(n.num));
    }

    #[test]
    fn mail_multilevel_lists() {
        // Outline (I. A. 1. a.): every level is a list style.
        let h = body(&list_doc(ListKind::Outline, &[("top", 0), ("sub", 1), ("subsub", 2)]));
        assert!(h.contains("<ol type=\"A\" style=\"list-style-type:upper-alpha\""), "{h}");
        assert!(h.contains("<ol type=\"1\" style=\"list-style-type:decimal\""), "{h}");
        // Legal (1. 1.1. 1.1.1.): the top level is a plain decimal list, the rest written out.
        let h = body(&list_doc(ListKind::Legal, &[("a", 0), ("b", 1), ("c", 2), ("d", 1), ("e", 0)]));
        assert!(h.starts_with("<ol type=\"1\" style=\"list-style-type:decimal\" data-mc-list=\"decimal:%1.|decimal:%1.%2.|"), "{h}");
        assert_eq!(h.matches("data-mc-list").count(), 1, "{h}");
        for want in ["<li>a<ol style=\"list-style-type:none\">", ">1.1.&nbsp;</span>b", ">1.1.1.&nbsp;</span>c", ">1.2.&nbsp;</span>d", "<li>e</li>"]
        {
            assert!(h.contains(want), "{want}: {h}");
        }
        assert_eq!(h.matches("<ol").count(), 3, "{h}");
        let f = parse(&h);
        let levels: Vec<u8> = f.blocks.iter().filter_map(|b| if let FBlock::Para(p) = b { p.list.map(|l| l.level) } else { None }).collect();
        assert_eq!(levels, vec![0, 1, 2, 1, 0]);
        let d = model::to_doc(&f);
        let n = d.body[1].as_para().and_then(|p| p.props.numbering).unwrap();
        assert_eq!(d.numbering.level(n.num, 1).map(|l| l.text.as_str()), Some("%1.%2."));
    }

    #[test]
    fn mail_numbering_continues_after_a_paragraph() {
        let mut d = list_doc(ListKind::Numbered, &[("one", 0), ("two", 0)]);
        let num = d.body[0].as_para().and_then(|p| p.props.numbering).unwrap().num;
        d.body.push(para_block(Paragraph::with_text("aside", CharProps::default())));
        let mut p = Paragraph::with_text("three", CharProps::default()).styled("ListParagraph");
        p.props.numbering = Some(NumRef { num, level: 0 });
        d.body.push(para_block(p));
        let h = body(&d);
        assert!(h.contains("<ol type=\"1\" start=\"3\" style=\"list-style-type:decimal\""), "{h}");
        // Read back: the new list starts at 3.
        let back = model::to_doc(&parse(&h));
        let n = back.body[3].as_para().and_then(|p| p.props.numbering).unwrap();
        assert_eq!(back.numbering.level(n.num, 0).map(|l| l.start), Some(3));
    }

    #[test]
    fn mail_plain_text_uses_the_lists_own_markers() {
        let t = String::from_utf8(crate::txt::export(&list_doc(ListKind::NumberedParen, &[("one", 0), ("two", 0)]))).unwrap();
        assert_eq!(t, "1)\tone\r\n2)\ttwo");
    }

    #[test]
    fn mail_plain_text_shows_all_caps_in_capitals() {
        let t =
            String::from_utf8(crate::txt::export(&doc_of(vec![Paragraph::with_text("shout", CharProps { caps: Some(true), ..Default::default() })])))
                .unwrap();
        assert_eq!(t, "SHOUT");
    }

    #[test]
    fn mail_hostile_css_never_panics() {
        for css in [
            "margin:;padding:1 2 3 4 5;border:999999pt double;line-height:-5;letter-spacing:NaNpt;vertical-align:1e40pt",
            "border-top:solid;border-left:thick rgb(1, 2, 3) dashed;text-indent:%;line-height:0%",
            "text-decoration:underline wavy;text-decoration-thickness:thick;text-shadow:none;-webkit-text-stroke:0",
        ] {
            let html = format!(
                "<ol data-mc-list=\"bogus:|:|decimal\" start=\"-4\"><li style=\"{css}\"><p style=\"{css}\"><span style=\"{css}\">x</span></p></ol>"
            );
            let _ = model::to_doc(&parse(&html));
        }
    }
}
