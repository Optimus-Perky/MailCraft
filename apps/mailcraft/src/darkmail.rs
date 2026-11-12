//! Dark reading for HTML mail, done the way Outlook and Gmail do it rather than by turning the
//! whole page round: each colour the message sets is changed in the message itself. Light
//! backgrounds go dark and dark text goes light, keeping their hue; then every element that has
//! colours of its own is checked against the background it actually sits on and its text is
//! made lighter (or darker) until it reads (WCAG contrast 4.5:1). Pictures are left alone, and
//! text over a background picture is judged against the colour the message gives for it.
//!
//! Everything but the colours is copied through untouched.

/// sRGB, 0-255.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// The page under a message that sets no background of its own, and its text.
pub const PAGE: Rgb = Rgb(0x1f, 0x1f, 0x1f);
pub const TEXT: Rgb = Rgb(0xe6, 0xe6, 0xe6);
const LINK: Rgb = Rgb(0x8a, 0xb4, 0xf8);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// Relative luminance (WCAG).
    pub fn luminance(self) -> f64 {
        let lin = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(self.0) + 0.7152 * lin(self.1) + 0.0722 * lin(self.2)
    }

    fn hsl(self) -> (f64, f64, f64) {
        let (r, g, b) = (self.0 as f64 / 255.0, self.1 as f64 / 255.0, self.2 as f64 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        if (max - min).abs() < 1e-9 {
            return (0.0, 0.0, l);
        }
        let d = max - min;
        let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
        let h = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h / 6.0, s, l)
    }

    fn from_hsl(h: f64, s: f64, l: f64) -> Rgb {
        let l = l.clamp(0.0, 1.0);
        let s = s.clamp(0.0, 1.0);
        if s < 1e-9 {
            let v = (l * 255.0).round() as u8;
            return Rgb(v, v, v);
        }
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        let f = |mut t: f64| {
            if t < 0.0 {
                t += 1.0;
            }
            if t > 1.0 {
                t -= 1.0;
            }
            let v = if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            };
            (v * 255.0).round().clamp(0.0, 255.0) as u8
        };
        Rgb(f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
    }
}

pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (x, y) = (a.luminance(), b.luminance());
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// A background for dark reading: light ones turned dark (white becomes the page colour),
/// dark ones kept.
pub fn dark_bg(c: Rgb) -> Rgb {
    if c.luminance() < 0.12 {
        return c;
    }
    let (h, s, l) = c.hsl();
    Rgb::from_hsl(h, s * 0.75, 0.12 + (1.0 - l) * 0.32)
}

/// Text for dark reading: dark text turned light, light text kept.
pub fn light_fg(c: Rgb) -> Rgb {
    if c.luminance() > 0.35 {
        return c;
    }
    let (h, s, l) = c.hsl();
    Rgb::from_hsl(h, s, 0.92 - l * 0.45)
}

/// A border for dark reading: faint light-mode lines stay faint, strong dark ones stay visible.
pub fn dark_border(c: Rgb) -> Rgb {
    let (h, s, l) = c.hsl();
    Rgb::from_hsl(h, s * 0.6, 0.3 + (1.0 - l) * 0.15)
}

/// Text that reads on this background: lightened (or, on a light background, darkened) until
/// the contrast is at least 4.5:1.
pub fn readable(fg: Rgb, bg: Rgb) -> Rgb {
    if contrast(fg, bg) >= 4.5 {
        return fg;
    }
    let (h, s, mut l) = fg.hsl();
    let up = bg.luminance() < 0.25;
    for _ in 0..40 {
        l = if up { (l + 0.025).min(1.0) } else { (l - 0.025).max(0.0) };
        let c = Rgb::from_hsl(h, s, l);
        if contrast(c, bg) >= 4.5 {
            return c;
        }
    }
    if up { Rgb(0xff, 0xff, 0xff) } else { Rgb(0, 0, 0) }
}

const NAMED: &[(&str, Rgb)] = &[
    ("white", Rgb(255, 255, 255)),
    ("black", Rgb(0, 0, 0)),
    ("red", Rgb(255, 0, 0)),
    ("green", Rgb(0, 128, 0)),
    ("blue", Rgb(0, 0, 255)),
    ("yellow", Rgb(255, 255, 0)),
    ("orange", Rgb(255, 165, 0)),
    ("purple", Rgb(128, 0, 128)),
    ("gray", Rgb(128, 128, 128)),
    ("grey", Rgb(128, 128, 128)),
    ("silver", Rgb(192, 192, 192)),
    ("navy", Rgb(0, 0, 128)),
    ("maroon", Rgb(128, 0, 0)),
    ("teal", Rgb(0, 128, 128)),
    ("olive", Rgb(128, 128, 0)),
    ("lime", Rgb(0, 255, 0)),
    ("aqua", Rgb(0, 255, 255)),
    ("fuchsia", Rgb(255, 0, 255)),
    ("darkgray", Rgb(169, 169, 169)),
    ("darkgrey", Rgb(169, 169, 169)),
    ("lightgray", Rgb(211, 211, 211)),
    ("lightgrey", Rgb(211, 211, 211)),
    ("whitesmoke", Rgb(245, 245, 245)),
    ("gainsboro", Rgb(220, 220, 220)),
    ("dimgray", Rgb(105, 105, 105)),
    ("dimgrey", Rgb(105, 105, 105)),
    ("darkred", Rgb(139, 0, 0)),
    ("darkblue", Rgb(0, 0, 139)),
    ("darkgreen", Rgb(0, 100, 0)),
    ("ivory", Rgb(255, 255, 240)),
    ("beige", Rgb(245, 245, 220)),
    ("snow", Rgb(255, 250, 250)),
    ("aliceblue", Rgb(240, 248, 255)),
];

/// A CSS or HTML colour: #rgb, #rrggbb (#rrggbbaa), rgb()/rgba(), or a common name.
pub fn parse_colour(v: &str) -> Option<Rgb> {
    let v = v.trim().trim_end_matches("!important").trim().to_ascii_lowercase();
    if let Some(h) = v.strip_prefix('#').or_else(|| (v.len() == 6 && v.chars().all(|c| c.is_ascii_hexdigit())).then_some(v.as_str())) {
        let n = |s: &str| u8::from_str_radix(s, 16).ok();
        return match h.len() {
            3 | 4 => {
                let d = |i: usize| h.get(i..i + 1).and_then(|c| n(&c.repeat(2)));
                Some(Rgb(d(0)?, d(1)?, d(2)?))
            }
            6 | 8 => Some(Rgb(n(h.get(0..2)?)?, n(h.get(2..4)?)?, n(h.get(4..6)?)?)),
            _ => None,
        };
    }
    if let Some(inner) = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb(")) {
        let inner = inner.trim_end_matches(')');
        let parts: Vec<&str> = inner.split([',', ' ', '/']).map(str::trim).filter(|p| !p.is_empty()).collect();
        let ch = |p: &str| -> Option<u8> {
            match p.strip_suffix('%') {
                Some(pc) => pc.parse::<f64>().ok().map(|x| (x * 2.55).round().clamp(0.0, 255.0) as u8),
                None => p.parse::<f64>().ok().map(|x| x.round().clamp(0.0, 255.0) as u8),
            }
        };
        // Mostly see-through: not a colour to judge by.
        if let Some(a) = parts.get(3).and_then(|a| a.parse::<f64>().ok())
            && a < 0.5
        {
            return None;
        }
        return Some(Rgb(ch(parts.first()?)?, ch(parts.get(1)?)?, ch(parts.get(2)?)?));
    }
    NAMED.iter().find(|(n, _)| *n == v).map(|(_, c)| *c)
}

/// The first colour in a value such as "#fff url(x.png) no-repeat" or "1px solid #ccc".
fn colour_in(v: &str) -> Option<(usize, usize, Rgb)> {
    let lower = v.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let rest = lower.get(i..).unwrap_or("");
        if rest.starts_with("url(") {
            i += rest.find(')').map(|k| k + 1).unwrap_or(rest.len());
            continue;
        }
        let at_word = i == 0 || !b.get(i - 1).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-' || *c == b'#');
        if at_word {
            let len = if rest.starts_with('#') {
                1 + rest.get(1..).unwrap_or("").chars().take_while(|c| c.is_ascii_hexdigit()).count()
            } else if rest.starts_with("rgb") {
                rest.find(')').map(|k| k + 1).unwrap_or(0)
            } else {
                rest.chars().take_while(|c| c.is_ascii_alphabetic()).count()
            };
            if len > 0
                && let Some(word) = v.get(i..i + len)
                && let Some(c) = parse_colour(word)
            {
                return Some((i, i + len, c));
            }
            i += len.max(1);
        } else {
            i += 1;
        }
    }
    None
}

fn replace_colour(v: &str, f: impl Fn(Rgb) -> Rgb) -> String {
    match colour_in(v) {
        Some((a, b, c)) => format!("{}{}{}", v.get(..a).unwrap_or(""), f(c).hex(), v.get(b..).unwrap_or("")),
        None => v.to_string(),
    }
}

/// One element's colours, as the message gives them.
#[derive(Default)]
struct Look {
    bg: Option<Rgb>,
    fg: Option<Rgb>,
    picture: bool,
}

/// Split a style attribute into (property, value) pairs; a ; inside brackets (a data: URL)
/// does not end a declaration.
fn declarations(style: &str) -> Vec<(String, String)> {
    let mut parts = vec![];
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in style.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ';' if depth == 0 => {
                parts.push(style.get(start..i).unwrap_or(""));
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(style.get(start..).unwrap_or(""));
    parts
        .into_iter()
        .filter_map(|d| {
            let (k, v) = d.split_once(':')?;
            Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

fn look_of(attrs: &[(String, Option<String>)], tag: &str) -> Look {
    let mut l = Look::default();
    let get = |n: &str| attrs.iter().find(|(k, _)| k == n).and_then(|(_, v)| v.as_deref());
    if let Some(c) = get("bgcolor").and_then(parse_colour) {
        l.bg = Some(c);
    }
    if get("background").is_some_and(|v| !v.trim().is_empty()) {
        l.picture = true;
    }
    if tag == "font"
        && let Some(c) = get("color").and_then(parse_colour)
    {
        l.fg = Some(c);
    }
    if tag == "body"
        && let Some(c) = get("text").and_then(parse_colour)
    {
        l.fg = Some(c);
    }
    for (k, v) in declarations(get("style").unwrap_or("")) {
        match k.as_str() {
            "background-color" => l.bg = parse_colour(&v).or(l.bg),
            "background" => {
                if let Some((_, _, c)) = colour_in(&v) {
                    l.bg = Some(c);
                }
                if v.to_ascii_lowercase().contains("url(") {
                    l.picture = true;
                }
            }
            "background-image" if v.to_ascii_lowercase().contains("url(") => l.picture = true,
            "color" => l.fg = parse_colour(&v).or(l.fg),
            _ => {}
        }
    }
    l
}

/// What an open element passes down: the background under its content, the text colour, and
/// whether a background picture is behind it.
#[derive(Clone, Copy)]
struct Ctx {
    bg: Rgb,
    fg: Rgb,
    picture: bool,
}

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"];

pub(crate) struct Tag<'a> {
    pub name: String,
    pub attrs: Vec<(String, Option<String>)>,
    pub self_close: bool,
    pub raw: &'a str,
}

/// Read a start tag at the beginning of `s` ("<td bgcolor=#fff style='...'>"). None if it
/// is not one.
pub(crate) fn read_tag(s: &str) -> Option<(Tag<'_>, usize)> {
    let b = s.as_bytes();
    if b.first() != Some(&b'<') || !b.get(1).is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut i = 1;
    while b.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-' || *c == b':') {
        i += 1;
    }
    let name = s.get(1..i)?.to_ascii_lowercase();
    let mut attrs = vec![];
    let mut self_close = false;
    loop {
        while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
            i += 1;
        }
        match b.get(i) {
            None => return None,
            Some(b'>') => {
                i += 1;
                break;
            }
            Some(b'/') => {
                i += 1;
                if b.get(i) == Some(&b'>') {
                    self_close = true;
                    i += 1;
                    break;
                }
                continue;
            }
            _ => {}
        }
        let a0 = i;
        while b.get(i).is_some_and(|c| !c.is_ascii_whitespace() && !matches!(c, b'=' | b'>' | b'/')) {
            i += 1;
        }
        let key = s.get(a0..i)?.to_ascii_lowercase();
        if key.is_empty() {
            i += 1;
            continue;
        }
        while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
            i += 1;
        }
        let mut val = None;
        if b.get(i) == Some(&b'=') {
            i += 1;
            while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                i += 1;
            }
            match b.get(i) {
                Some(&q) if q == b'"' || q == b'\'' => {
                    let v0 = i + 1;
                    let e = s.get(v0..).and_then(|r| r.find(q as char)).map(|k| v0 + k)?;
                    val = Some(s.get(v0..e)?.to_string());
                    i = e + 1;
                }
                _ => {
                    let v0 = i;
                    while b.get(i).is_some_and(|c| !c.is_ascii_whitespace() && *c != b'>') {
                        i += 1;
                    }
                    val = Some(s.get(v0..i)?.to_string());
                }
            }
        }
        if attrs.len() < 128 {
            attrs.push((key, val));
        }
    }
    Some((Tag { name, attrs, self_close, raw: s.get(..i)? }, i))
}

pub(crate) fn write_tag(t: &Tag<'_>, out: &mut String) {
    out.push('<');
    out.push_str(&t.name);
    for (k, v) in &t.attrs {
        out.push(' ');
        out.push_str(k);
        if let Some(v) = v {
            out.push_str("=\"");
            out.push_str(&v.replace('"', "&quot;"));
            out.push('"');
        }
    }
    if t.self_close {
        out.push_str(" /");
    }
    out.push('>');
}

pub(crate) fn set_attr(attrs: &mut Vec<(String, Option<String>)>, key: &str, val: String) {
    match attrs.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = Some(val),
        None => attrs.push((key.to_string(), Some(val))),
    }
}

/// Recolour one start tag for dark reading; returns what its content inherits.
fn recolour(tag: &mut Tag<'_>, parent: Ctx) -> Ctx {
    let look = look_of(&tag.attrs, &tag.name);
    // Under or on a background picture: the picture keeps its colours, so the colour given
    // with it does too, and text is judged against that.
    let picture = parent.picture || look.picture;
    let bg = match look.bg {
        Some(c) if picture => c,
        Some(c) => dark_bg(c),
        None => parent.bg,
    };
    let own_fg = look.fg.map(|c| if picture { c } else { light_fg(c) });
    let mut fg = own_fg.unwrap_or(parent.fg);
    // Only where something changed: an element with colours of its own, or new background.
    let mut set_fg = own_fg.is_some();
    if look.bg.is_some() || own_fg.is_some() {
        let r = readable(fg, bg);
        if r != fg {
            fg = r;
            set_fg = true;
        }
    }
    if picture && look.fg.is_none() && look.bg.is_none() {
        // Nothing known about the picture: leave the message's own colours alone.
        return Ctx { bg, fg, picture };
    }
    // Write the colours back (the style attribute only when a colour in it changed).
    let style = tag.attrs.iter().find(|(k, _)| k == "style").and_then(|(_, v)| v.clone()).unwrap_or_default();
    let mut decl = declarations(&style);
    let mut changed = false;
    for (k, v) in decl.iter_mut() {
        let new = match k.as_str() {
            "background-color" | "background" if !picture => replace_colour(v, dark_bg),
            "color" if parse_colour(v).is_some() => format!("{}{}", fg.hex(), if v.contains("!important") { " !important" } else { "" }),
            k if k.starts_with("border") && !picture => replace_colour(v, dark_border),
            _ => v.clone(),
        };
        if new != *v {
            *v = new;
            changed = true;
        }
    }
    if set_fg && !decl.iter().any(|(k, v)| k == "color" && parse_colour(v).is_some()) {
        decl.push(("color".into(), fg.hex()));
        changed = true;
    }
    if changed {
        set_attr(&mut tag.attrs, "style", decl.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(";"));
    }
    if !picture && let Some(c) = look.bg.filter(|_| tag.attrs.iter().any(|(k, _)| k == "bgcolor")) {
        set_attr(&mut tag.attrs, "bgcolor", dark_bg(c).hex());
    }
    if tag.name == "font" && tag.attrs.iter().any(|(k, _)| k == "color") {
        set_attr(&mut tag.attrs, "color", fg.hex());
    }
    if tag.name == "body" {
        for (k, f) in [("text", fg), ("link", LINK), ("vlink", LINK)] {
            if tag.attrs.iter().any(|(a, _)| a == k) {
                set_attr(&mut tag.attrs, k, f.hex());
            }
        }
    }
    Ctx { bg, fg, picture }
}

/// The colours in a style sheet, mapped the same way (no element to judge them against).
/// Only colour properties inside { } blocks are touched, so selectors such as a:hover are not.
fn recolour_css(css: &str) -> String {
    let lower = css.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut out = String::with_capacity(css.len());
    let (mut i, mut copied, mut depth) = (0usize, 0usize, 0i32);
    while i < b.len() {
        match b.get(i) {
            Some(b'{') => depth += 1,
            Some(b'}') => depth = (depth - 1).max(0),
            _ => {}
        }
        let boundary = i == 0 || b.get(i - 1).is_some_and(|c| matches!(c, b'{' | b';' | b' ' | b'\n' | b'\r' | b'\t'));
        if depth > 0 && boundary && b.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
            let mut j = i;
            while b.get(j).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-') {
                j += 1;
            }
            let prop = lower.get(i..j).unwrap_or("");
            let mut k = j;
            while b.get(k).is_some_and(|c| c.is_ascii_whitespace()) {
                k += 1;
            }
            let colourful = matches!(prop, "color" | "background" | "background-color") || prop.starts_with("border");
            if colourful && b.get(k) == Some(&b':') {
                // The value runs to ; or } outside brackets.
                let v0 = k + 1;
                let mut e = v0;
                let mut paren = 0;
                while let Some(c) = b.get(e) {
                    match c {
                        b'(' => paren += 1,
                        b')' => paren -= 1,
                        b';' | b'}' if paren <= 0 => break,
                        _ => {}
                    }
                    e += 1;
                }
                let value = css.get(v0..e).unwrap_or("");
                let mapped = match prop {
                    "color" => replace_colour(value, light_fg),
                    _ if value.to_ascii_lowercase().contains("url(") => value.to_string(),
                    p if p.starts_with("border") => replace_colour(value, dark_border),
                    _ => replace_colour(value, dark_bg),
                };
                out.push_str(css.get(copied..v0).unwrap_or(""));
                out.push_str(&mapped);
                copied = e;
                i = e;
                continue;
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    out.push_str(css.get(copied..).unwrap_or(""));
    out
}

/// The message, recoloured for dark reading, with the page's own colours first.
pub fn rewrite(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + 256);
    out.push_str(&format!("<style>html,body{{background:{};color:{}}} a{{color:{}}}</style>", PAGE.hex(), TEXT.hex(), LINK.hex()));
    let mut stack: Vec<(String, Ctx)> = vec![];
    let page = Ctx { bg: PAGE, fg: TEXT, picture: false };
    let mut i = 0;
    let b = html.as_bytes();
    while i < b.len() {
        let rest = html.get(i..).unwrap_or("");
        let Some(lt) = rest.find('<') else {
            out.push_str(rest);
            break;
        };
        out.push_str(rest.get(..lt).unwrap_or(""));
        i += lt;
        let rest = html.get(i..).unwrap_or("");
        if rest.starts_with("<!--") {
            let end = rest.find("-->").map(|k| k + 3).unwrap_or(rest.len());
            out.push_str(rest.get(..end).unwrap_or(""));
            i += end;
            continue;
        }
        if let Some(close) = rest.strip_prefix("</") {
            let name: String =
                close.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':').collect::<String>().to_ascii_lowercase();
            if let Some(at) = stack.iter().rposition(|(n, _)| *n == name) {
                stack.truncate(at);
            }
            let end = rest.find('>').map(|k| k + 1).unwrap_or(rest.len());
            out.push_str(rest.get(..end).unwrap_or(""));
            i += end;
            continue;
        }
        match read_tag(rest) {
            Some((mut tag, len)) => {
                let parent = stack.last().map(|(_, c)| *c).unwrap_or(page);
                if tag.name == "style" {
                    // The style sheet runs to </style>.
                    out.push_str(tag.raw);
                    let body = html.get(i + len..).unwrap_or("");
                    let end = body.to_ascii_lowercase().find("</style").unwrap_or(body.len());
                    out.push_str(&recolour_css(body.get(..end).unwrap_or("")));
                    i += len + end;
                    continue;
                }
                let ctx = recolour(&mut tag, parent);
                write_tag(&tag, &mut out);
                if !tag.self_close && !VOID.contains(&tag.name.as_str()) && stack.len() < 512 {
                    stack.push((tag.name.clone(), ctx));
                }
                i += len;
            }
            None => {
                out.push('<');
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colour_of(html: &str, marker: &str) -> Rgb {
        // The colour written onto the element whose text is `marker`.
        let at = html.find(marker).unwrap();
        let open = html.get(..at).unwrap().rfind('<').unwrap();
        let tag = html.get(open..at).unwrap();
        // The text colour, not background-color.
        let at = tag.match_indices("color:").find(|(i, _)| *i == 0 || !tag[..*i].ends_with('-')).unwrap().0;
        parse_colour(tag.get(at + 6..at + 13).unwrap()).unwrap()
    }

    #[test]
    fn colours_parse() {
        assert_eq!(parse_colour("#fff"), Some(Rgb(255, 255, 255)));
        assert_eq!(parse_colour("#1A2b3C"), Some(Rgb(0x1a, 0x2b, 0x3c)));
        assert_eq!(parse_colour("rgb(10, 20, 30)"), Some(Rgb(10, 20, 30)));
        assert_eq!(parse_colour("rgba(10,20,30,0.1)"), None);
        assert_eq!(parse_colour("White !important"), Some(Rgb(255, 255, 255)));
        assert_eq!(parse_colour("inherit"), None);
        assert_eq!(colour_in("#ffeeee url(a.png) no-repeat").map(|c| c.2), Some(Rgb(0xff, 0xee, 0xee)));
        assert_eq!(colour_in("1px solid #cccccc").map(|c| c.2), Some(Rgb(0xcc, 0xcc, 0xcc)));
    }

    #[test]
    fn light_mail_goes_dark_and_reads() {
        // A pale pink panel with dark red text (the shop's birthday mail), a white table with
        // grey text, and a link.
        let html = r##"<table bgcolor="#ffffff"><tr><td style="background-color:#f6dcdc;color:#6b1d1d">Five years</td></tr>
            <tr><td><p style="color:#999999">Small print</p><a href="x" style="color:#0645ad">Shop</a></td></tr></table>"##;
        let out = rewrite(html);
        assert!(out.contains("bgcolor=\"#1f1f1f\""), "{out}");
        let pink = colour_in(out.split("Five years").next().unwrap().rsplit("background-color:").next().unwrap()).unwrap().2;
        assert!(pink.luminance() < 0.1, "panel now dark: {pink:?}");
        for marker in ["Five years", "Small print", "Shop"] {
            let fg = colour_of(&out, marker);
            let bg = if marker == "Five years" { pink } else { PAGE };
            assert!(contrast(fg, bg) >= 4.5, "{marker}: {fg:?} on {bg:?}");
        }
    }

    #[test]
    fn dark_text_on_a_dark_panel_is_lifted() {
        // A designer's dark panel with text that barely shows even in light mode.
        let out = rewrite(r##"<div style="background:#5a1e24"><p style="color:#3a1010">Thank you</p><h2 style="color:#000">Follow us</h2></div>"##);
        for m in ["Thank you", "Follow us"] {
            assert!(contrast(colour_of(&out, m), Rgb(0x5a, 0x1e, 0x24)) >= 4.5, "{m}: {out}");
        }
    }

    #[test]
    fn background_pictures_keep_their_colours() {
        // Text over a picture is judged against the colour given for it, not darkened.
        let out = rewrite(r##"<td style="background:#5a1e24 url(https://x/bg.jpg)"><span style="color:#2a0a0a">Over the picture</span></td>"##);
        assert!(out.contains("#5a1e24 url(https://x/bg.jpg)"), "{out}");
        assert!(contrast(colour_of(&out, "Over the picture"), Rgb(0x5a, 0x1e, 0x24)) >= 4.5);
        // A picture with no colour given: left as the sender made it.
        let raw = r##"<td background="bg.jpg"><span style="color:#ffffff">White on picture</span></td>"##;
        assert!(rewrite(raw).contains(r##"<span style="color:#ffffff">"##));
    }

    #[test]
    fn already_dark_mail_is_left_alone_and_the_rest_is_copied() {
        let html = r##"<!-- hi --><div style="background-color:#111111;color:#eeeeee;padding:4px">Night &amp; day <b>bold</b></div><br/><img src="a.png" alt="x">"##;
        let out = rewrite(html);
        assert!(out.contains("background-color:#111111") && out.contains("color:#eeeeee") && out.contains("padding:4px"));
        assert!(out.contains("Night &amp; day <b>bold</b>") && out.contains("<!-- hi -->") && out.contains("<img src=\"a.png\" alt=\"x\">"));
    }

    #[test]
    fn style_sheets_and_old_attributes() {
        let out = rewrite(
            r##"<style>.box { background-color: #ffffff; color: #333; border: 1px solid #dddddd }</style><font color="#000080">Old</font><body text="black" link="blue">"##,
        );
        assert!(out.contains("background-color: #1f1f1f"), "{out}");
        assert!(!out.contains("color: #333;") && !out.contains("#dddddd"), "{out}");
        let border = colour_in(out.split("border:").nth(1).unwrap()).unwrap().2;
        assert!(border.luminance() < 0.12 && border.luminance() > 0.03, "faint border stays faint: {border:?}");
        let font = out.split("<font color=\"").nth(1).unwrap().get(..7).unwrap();
        assert!(contrast(parse_colour(font).unwrap(), PAGE) >= 4.5, "{out}");
        assert!(out.contains("text=\"#") && !out.contains("text=\"black\""), "{out}");
    }

    #[test]
    fn data_urls_and_hover_rules_survive() {
        let html = r##"<div style="background-image:url(data:image/png;base64,AAAA);color:#000">x</div><style>a:hover{color:#000} .c{background:url(a.png)}</style>"##;
        let out = rewrite(html);
        assert!(out.contains("url(data:image/png;base64,AAAA)"), "{out}");
        assert!(out.contains("a:hover{color:#") && !out.contains("a:hover{color:#000}"), "{out}");
        assert!(out.contains(".c{background:url(a.png)}"), "{out}");
    }

    #[test]
    fn hostile_input_never_panics() {
        for s in ["<", "<a", "<a b=\"", "</", "<style>", "<td style='color:", "<<<>>>", "<p style=\"background:url(\">x", "\u{1F600}<b>\u{e9}</b>"] {
            let _ = rewrite(s);
        }
    }
}
