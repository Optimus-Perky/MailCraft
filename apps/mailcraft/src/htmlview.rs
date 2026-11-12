//! Showing a mail's HTML as the sender meant it: the system's own browser engine (WebView2 on
//! Windows, WebKitGTK on Linux) in a child view laid over the reading pane.
//!
//! Mail is hostile input, so every message is prepared before it is shown:
//! - a Content-Security-Policy goes first: no scripts, no forms, no plugins, no frames, and no
//!   remote images until the reader asks for them (tracking pixels, like Outlook);
//! - `cid:` references (pictures sent inside the mail) become `data:` URIs;
//! - any navigation away from the message (a clicked link) is cancelled and handed to the
//!   default browser instead.
//!
//! Once the reader clicks into a message the view has the keyboard, and MailCraft's shortcuts
//! (Ctrl+W, Delete, Ctrl+R...) went nowhere (2026-10-29). MailCraft's own small script
//! hands those keys back; copying, selecting and zooming stay with the message. The policy lets
//! that one script run by a random code (a nonce) made for each message, so a script in the
//! mail itself still cannot.

use base64::Engine;
use regex::Regex;
use std::sync::LazyLock;

/// A picture carried inside the mail: its Content-ID, MIME type and bytes.
pub struct Inline {
    pub cid: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

static REMOTE_IMG: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(?i)(<img[^>]+src\s*=\s*["']?\s*https?:|url\(\s*["']?\s*https?:|background\s*=\s*["']?\s*https?:)"#).ok());

/// Does the message pull pictures from the internet?
pub fn has_remote_images(html: &str) -> bool {
    REMOTE_IMG.as_ref().is_some_and(|r| r.is_match(html))
}

/// The message, ready to show: the policy first, inline pictures resolved.
/// `theme_css` (dark reading, high contrast) goes last, so it wins over the message's own style.
pub fn prepare(html: &str, inline: &[Inline], allow_remote: bool, theme_css: &str) -> String {
    let img = if allow_remote { "img-src data: https: http:" } else { "img-src data:" };
    // No random code, no script at all: the keys then simply stay with the message.
    let (scripts, ours) = match nonce() {
        Some(n) => (format!("'nonce-{n}'"), format!("<script nonce=\"{n}\">{KEYS_SCRIPT}</script>")),
        None => ("'none'".to_string(), String::new()),
    };
    let csp = format!(
        "default-src 'none'; {img}; style-src 'unsafe-inline' data:; font-src data:; media-src 'none'; script-src {scripts}; object-src 'none'; frame-src 'none'; form-action 'none'; base-uri 'none'"
    );
    let mut body = html.to_string();
    for p in inline {
        let data = format!("data:{};base64,{}", p.mime, base64::engine::general_purpose::STANDARD.encode(&p.bytes));
        let cid = p.cid.trim_matches(['<', '>']);
        for form in [format!("cid:{cid}"), format!("CID:{cid}")] {
            body = body.replace(&form, &data);
        }
    }
    // Before <html>: the parser puts these in <head> ahead of anything that loads.
    format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\"><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"light\">\
{ours}\
         <style>html{{background:#fff}} body{{margin:14px 18px; font-family:'Segoe UI',Arial,sans-serif; font-size:14px; overflow-wrap:anywhere}} img{{max-width:100%;height:auto}}</style>{body}{}",
        if theme_css.is_empty() { String::new() } else { format!("<style>{theme_css}</style>") }
    ) + &format!("<style>{}</style>", crate::links::CSS)
}

/// A fresh random code for the one script a message may run (MailCraft's own); None when
/// there is no randomness to make one from.
fn nonce() -> Option<String> {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut b).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(b))
}

/// Plain-text mail as HTML that keeps its line breaks.
pub fn from_text(text: &str) -> String {
    let esc = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!("<pre style=\"white-space:pre-wrap;font-family:Consolas,'Courier New',monospace;font-size:13px\">{esc}</pre>")
}

/// The browser view itself (Windows and Linux).
#[cfg(any(windows, target_os = "linux"))]
pub struct HtmlView {
    view: wry::WebView,
    _context: wry::WebContext,
    /// Keys pressed in the view that belong to MailCraft, waiting for the next frame.
    keys: std::sync::Arc<std::sync::Mutex<Vec<egui::Event>>>,
    loaded: std::rc::Rc<std::cell::Cell<bool>>,
    shown: Option<String>,
    visible: bool,
    /// Where it was last put (x, y, width, height in physical pixels).
    bounds: Option<(i32, i32, u32, u32)>,
}

#[cfg(any(windows, target_os = "linux"))]
impl HtmlView {
    pub fn new(window: &impl wry::raw_window_handle::HasWindowHandle, ctx: egui::Context) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        gtk::init().map_err(|e| e.to_string())?;
        // WebView2 keeps its profile beside the exe unless told otherwise, and that fails
        // (Access is denied) when the exe runs from a share or Program Files.
        let dir = crate::accounts::cache_path().with_file_name("WebView2");
        let _ = std::fs::create_dir_all(&dir);
        let mut context = wry::WebContext::new(Some(dir));
        // A page that finished loading may have taken the keyboard; `show` gives it back.
        let loaded = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = loaded.clone();
        let keys = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (sink, repaint) = (keys.clone(), ctx.clone());
        let view = wry::WebViewBuilder::new_with_web_context(&mut context)
            // Ctrl+scroll (and Ctrl + plus, minus, 0) zooms the message: WebView2's own zoom,
            // off unless asked for (2026-10-29: "sometimes I need to zoom in, but can't").
            .with_hotkeys_zoom(true)
            .with_ipc_handler(move |req: wry::http::Request<String>| {
                log::debug!("message view: {}", req.body());
                // Once a run: the evidence that the message's key script runs on this PC.
                static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if req.body() == "ready" && !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    log::info!("the message view hands MailCraft's keys back (Ctrl+W, Delete...)");
                }
                if let Some(ev) = key_event(req.body())
                    && let Ok(mut k) = sink.lock()
                {
                    k.push(ev);
                    repaint.request_repaint();
                }
            })
            .with_on_page_load_handler(move |event, _url| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    flag.set(true);
                    ctx.request_repaint();
                }
            })
            .with_html("<html></html>")
            .with_visible(false)
            .with_devtools(false)
            .with_navigation_handler(|url: String| {
                // The message itself loads as about:blank or data:; anything else is a click.
                if url.starts_with("about:") || url.starts_with("data:") {
                    return true;
                }
                open_externally(&url);
                false
            })
            .with_new_window_req_handler(|url: String, _features| {
                open_externally(&url);
                wry::NewWindowResponse::Deny
            })
            .build_as_child(window)
            .map_err(|e| e.to_string())?;
        Ok(HtmlView { view, _context: context, keys, loaded, shown: None, visible: false, bounds: None })
    }

    /// Show `html` (already prepared) over `rect`, given in physical pixels.
    pub fn show(&mut self, html: &str, x: f32, y: f32, w: f32, h: f32) {
        if self.shown.as_deref() != Some(html) {
            let _ = self.view.load_html(html);
            // Loading a page can pull the keyboard into the view: give it back, so the list's
            // keys (arrows, Delete, Ctrl+A...) keep working; again when the load finishes.
            let _ = self.view.focus_parent();
            self.shown = Some(html.to_string());
        }
        // Only when it moved or changed size: on Windows each move is a real window move, which
        // brings another frame, which moved it again: MailCraft and Explorer kept busy for ever.
        let bounds = (x as i32, y as i32, w.max(1.0) as u32, h.max(1.0) as u32);
        if self.bounds != Some(bounds) {
            let _ = self.view.set_bounds(wry::Rect {
                position: wry::dpi::PhysicalPosition::new(bounds.0, bounds.1).into(),
                size: wry::dpi::PhysicalSize::new(bounds.2, bounds.3).into(),
            });
            self.bounds = Some(bounds);
        }
        if self.loaded.replace(false) {
            let _ = self.view.focus_parent();
        }
        if !self.visible {
            let _ = self.view.set_visible(true);
            // Showing it must not take the keyboard from the message list (shortcuts, Ctrl+A).
            let _ = self.view.focus_parent();
            self.visible = true;
        }
    }

    /// Keys pressed in the view that are MailCraft's: the keyboard goes back to MailCraft with
    /// them, so the next ones reach it directly.
    pub fn take_keys(&self) -> Vec<egui::Event> {
        let keys = self.keys.lock().map(|mut k| std::mem::take(&mut *k)).unwrap_or_default();
        if !keys.is_empty() {
            let _ = self.view.focus_parent();
        }
        keys
    }

    /// The system's print dialog for what the view shows.
    pub fn print(&self) -> Result<(), String> {
        self.view.print().map_err(|e| e.to_string())
    }

    pub fn hide(&mut self) {
        if self.visible {
            let _ = self.view.set_visible(false);
            self.visible = false;
        }
    }
}

/// MailCraft's script in each message (by the nonce): hands MailCraft's keys back, through
/// the browser's own message channel (wry's `window.ipc` wrapper is itself an added script,
/// and those did not run in a page loaded from text in the test). Kept by the message: copy and select all (Ctrl+C, Ctrl+A, Ctrl+Insert), zoom (Ctrl with
/// plus, minus, 0), and plain typing and arrows, which scroll it. Everything else with Ctrl or
/// Alt, and Delete, Escape and the function keys, are MailCraft's. The mouse's back and
/// forward buttons come back as Alt+Left / Alt+Right (back and forward between folders).
const KEYS_SCRIPT: &str = r#"(function () {
  var post = (window.chrome && window.chrome.webview) ? function (m) { window.chrome.webview.postMessage(m); }
    : (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ipc) ? function (m) { window.webkit.messageHandlers.ipc.postMessage(m); }
    : null;
  if (!post) return;
  var mine = ['KeyC', 'KeyA', 'Insert', 'Equal', 'Minus', 'Digit0', 'NumpadAdd', 'NumpadSubtract', 'Numpad0'];
  document.addEventListener('keydown', function (e) {
    var ours = (e.ctrlKey || e.altKey) ? mine.indexOf(e.code) < 0 : /^(Delete|Escape|F[0-9]+)$/.test(e.code);
    if (!ours || /^(Control|Shift|Alt|Meta)/.test(e.code)) return;
    e.preventDefault();
    e.stopPropagation();
    post('key:' + (e.ctrlKey ? 1 : 0) + (e.shiftKey ? 1 : 0) + (e.altKey ? 1 : 0) + ':' + e.code);
  }, true);
  var side = function (e) { return e.button === 3 || e.button === 4; };
  document.addEventListener('mousedown', function (e) { if (side(e)) e.preventDefault(); }, true);
  document.addEventListener('mouseup', function (e) {
    if (!side(e)) return;
    e.preventDefault();
    e.stopPropagation();
    post('key:001:' + (e.button === 3 ? 'ArrowLeft' : 'ArrowRight'));
  }, true);
  post('ready');
})();"#;

/// A key handed back by [`KEYS_SCRIPT`] ("key:<ctrl><shift><alt>:<code>") as egui's own.
#[cfg(any(windows, target_os = "linux"))]
fn key_event(msg: &str) -> Option<egui::Event> {
    let rest = msg.strip_prefix("key:")?;
    let (mods, code) = rest.split_once(':')?;
    let mut m = mods.chars().map(|c| c == '1');
    let (ctrl, shift, alt) = (m.next()?, m.next()?, m.next()?);
    let key = egui::Key::from_name(code.strip_prefix("Digit").unwrap_or(code))?;
    let modifiers = egui::Modifiers { alt, ctrl, shift, mac_cmd: false, command: ctrl };
    Some(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers })
}

/// Linux: WebKitGTK needs GTK's events pumped; call once a frame.
pub fn pump() {
    #[cfg(target_os = "linux")]
    while gtk::is_initialized() && gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

fn open_externally(url: &str) {
    #[cfg(windows)]
    {
        if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
            let _ = open::that_detached(url);
        }
    }
    #[cfg(not(windows))]
    log::info!("link clicked: {url}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn keys_from_the_view_become_egui_keys() {
        let Some(egui::Event::Key { key, modifiers, pressed, .. }) = key_event("key:100:KeyW") else { panic!("Ctrl+W not read") };
        assert_eq!(key, egui::Key::W);
        assert!(pressed && modifiers.ctrl && modifiers.command && !modifiers.shift && !modifiers.alt);
        let Some(egui::Event::Key { key, modifiers, .. }) = key_event("key:110:Tab") else { panic!("Ctrl+Shift+Tab not read") };
        assert_eq!(key, egui::Key::Tab);
        assert!(modifiers.ctrl && modifiers.shift);
        assert!(matches!(key_event("key:000:Delete"), Some(egui::Event::Key { key: egui::Key::Delete, .. })));
        assert!(matches!(key_event("key:100:Digit1"), Some(egui::Event::Key { key: egui::Key::Num1, .. })));
        // F5, and the mouse's back button as the script hands it back (Alt+Left).
        assert!(matches!(key_event("key:000:F5"), Some(egui::Event::Key { key: egui::Key::F5, modifiers, .. }) if modifiers.is_none()));
        let Some(egui::Event::Key { key, modifiers, .. }) = key_event("key:001:ArrowLeft") else { panic!("Alt+Left not read") };
        assert!(key == egui::Key::ArrowLeft && modifiers.alt && !modifiers.ctrl && !modifiers.shift);
        for junk in ["", "key:", "key:1:KeyW", "key:100:NoSuchKey", "click:100:KeyW", "key:10x"] {
            assert!(key_event(junk).is_none(), "{junk:?}");
        }
    }

    #[test]
    fn policy_first_cid_inlined_remote_detected() {
        let html = r#"<html><body><img src="cid:logo@x"><img src="https://tracker.example/p.gif"><script>alert(1)</script></body></html>"#;
        assert!(has_remote_images(html));
        assert!(!has_remote_images("<img src='data:image/png;base64,AA'>"));
        let out = prepare(html, &[Inline { cid: "<logo@x>".into(), mime: "image/png".into(), bytes: vec![1, 2, 3] }], false, "");
        assert!(out.starts_with("<meta http-equiv=\"Content-Security-Policy\""));
        // Only MailCraft's own script, by its nonce; the mail's <script> has none.
        let nonce = out.split("script-src 'nonce-").nth(1).and_then(|r| r.split('\'').next()).unwrap();
        assert!(nonce.len() >= 20 && out.contains(&format!("<script nonce=\"{nonce}\">")) && out.contains("<script>alert(1)</script>"));
        assert!(out.contains("img-src data:;"));
        assert_ne!(
            nonce,
            prepare(html, &[], false, "").split("script-src 'nonce-").nth(1).and_then(|r| r.split('\'').next()).unwrap(),
            "a new nonce each time"
        );
        assert!(out.contains("data:image/png;base64,AQID") && !out.contains("cid:logo"));
        assert!(prepare(html, &[], true, "").contains("img-src data: https: http:"));
        assert_eq!(
            from_text("a < b\n"),
            "<pre style=\"white-space:pre-wrap;font-family:Consolas,'Courier New',monospace;font-size:13px\">a &lt; b\n</pre>"
        );
    }
}
