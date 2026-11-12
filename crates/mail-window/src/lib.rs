//! Finding one of MailCraft's own windows by its title, and lending its native handle.
//!
//! egui opens extra windows (a message popped out of the main one) but hands out a native
//! handle only for the main window; the browser view that shows a message's HTML has to go
//! inside a native window. Each window's title is its own, so it is found by that.
//! This needs the operating system's own calls (unsafe), so it lives here, apart from the app
//! (which forbids unsafe code).

/// One of our own windows found by its title (a message popped out into a window of its own,
/// which egui opens but does not hand out a native handle for), so a browser view can go in it.
pub struct Native(raw_window_handle::RawWindowHandle);

impl raw_window_handle::HasWindowHandle for Native {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        // The window lives as long as its popped-out message: the view is dropped first.
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(self.0) })
    }
}

/// Our window titled `title` that is not one of `taken` (an id per window), if it is open yet.
#[cfg(windows)]
pub fn find_window(title: &str, taken: &[u64]) -> Option<(Native, u64)> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, GetWindowTextW};
    struct Find<'a> {
        title: Vec<u16>,
        taken: &'a [u64],
        found: Option<u64>,
    }
    unsafe extern "system" fn each(hwnd: HWND, l: LPARAM) -> i32 {
        let f = unsafe { &mut *(l as *mut Find) };
        let mut buf = [0u16; 1024];
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        let id = hwnd as usize as u64;
        if n > 0 && buf[..n as usize] == f.title[..] && !f.taken.contains(&id) {
            f.found = Some(id);
            return 0;
        }
        1
    }
    let mut f = Find { title: title.encode_utf16().collect(), taken, found: None };
    // egui makes every window on this (the UI) thread.
    unsafe { EnumThreadWindows(GetCurrentThreadId(), Some(each), &mut f as *mut Find as LPARAM) };
    let id = f.found?;
    let h = raw_window_handle::Win32WindowHandle::new(std::num::NonZeroIsize::new(id as isize)?);
    Some((Native(h.into()), id))
}

/// Linux (X11): the same, through the X server's window tree (the test rig).
#[cfg(target_os = "linux")]
// xlib::Window is a C unsigned long: u64 here, narrower elsewhere, so the casts stay.
#[allow(clippy::unnecessary_cast)]
pub fn find_window(title: &str, taken: &[u64]) -> Option<(Native, u64)> {
    use x11_dl::xlib;
    let x = xlib::Xlib::open().ok()?;
    unsafe {
        let d = (x.XOpenDisplay)(std::ptr::null());
        if d.is_null() {
            return None;
        }
        let utf8 = (x.XInternAtom)(d, c"UTF8_STRING".as_ptr(), 0);
        let net_name = (x.XInternAtom)(d, c"_NET_WM_NAME".as_ptr(), 0);
        let name_of = |w: xlib::Window| -> Option<String> {
            let (mut kind, mut format, mut n, mut after, mut data) = (0, 0, 0, 0, std::ptr::null_mut());
            let ok = (x.XGetWindowProperty)(d, w, net_name, 0, 1024, 0, utf8, &mut kind, &mut format, &mut n, &mut after, &mut data);
            if ok != 0 || data.is_null() {
                return None;
            }
            let s = String::from_utf8_lossy(std::slice::from_raw_parts(data, n as usize)).into_owned();
            (x.XFree)(data as *mut _);
            Some(s)
        };
        let mut stack = vec![(x.XDefaultRootWindow)(d)];
        let mut found = None;
        while let Some(w) = stack.pop() {
            if name_of(w).as_deref() == Some(title) && !taken.contains(&(w as u64)) {
                found = Some(w);
                break;
            }
            let (mut root, mut parent, mut kids, mut n) = (0, 0, std::ptr::null_mut(), 0);
            if (x.XQueryTree)(d, w, &mut root, &mut parent, &mut kids, &mut n) != 0 && !kids.is_null() {
                stack.extend_from_slice(std::slice::from_raw_parts(kids, n as usize));
                (x.XFree)(kids as *mut _);
            }
        }
        (x.XCloseDisplay)(d);
        let w = found?;
        Some((Native(raw_window_handle::XlibWindowHandle::new(w).into()), w as u64))
    }
}

/// Where the mouse pointer is on the whole desktop, in physical pixels (a tab dropped on another
/// screen is let go outside the window, where the app no longer sees the pointer).
#[cfg(windows)]
pub fn cursor_pos() -> Option<(f64, f64)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x as f64, p.y as f64))
}

#[cfg(target_os = "linux")]
pub fn cursor_pos() -> Option<(f64, f64)> {
    use x11_dl::xlib;
    let x = xlib::Xlib::open().ok()?;
    unsafe {
        let d = (x.XOpenDisplay)(std::ptr::null());
        if d.is_null() {
            return None;
        }
        let root = (x.XDefaultRootWindow)(d);
        let (mut r, mut c, mut rx, mut ry, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0, 0, 0);
        let ok = (x.XQueryPointer)(d, root, &mut r, &mut c, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask);
        (x.XCloseDisplay)(d);
        (ok != 0).then_some((rx as f64, ry as f64))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn cursor_pos() -> Option<(f64, f64)> {
    None
}

/// Elsewhere: never found (the app falls back to its own renderer).
#[cfg(not(any(windows, target_os = "linux")))]
pub fn find_window(_title: &str, _taken: &[u64]) -> Option<(Native, u64)> {
    None
}
