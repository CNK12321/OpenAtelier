//! Keeping the keyboard on the editor's window (Windows).
//!
//! When the window is the active one but *no* window has keyboard focus (something
//! took it and went away), Windows sends every key as a "system" keystroke: winit still
//! passes them on, so typing works, but each one makes the default beep, and egui —
//! told the window lost focus — hides the text caret. Only switching away and back
//! fixed it. Each frame this checks for that state and gives the focus back, logging
//! once what had it, so the culprit can be found.

use eframe::egui;

#[cfg(windows)]
pub fn keep(frame: &eframe::Frame, ctx: &egui::Context) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow};

    let Ok(handle) = frame.window_handle() else { return };
    let RawWindowHandle::Win32(w) = handle.as_raw() else { return };
    let hwnd = HWND(w.hwnd.get() as *mut _);
    // SAFETY: plain Win32 queries and SetFocus on our own window, on its own thread.
    unsafe {
        if GetForegroundWindow() != hwnd {
            return;
        }
        let focus = GetFocus();
        if focus == hwnd {
            return;
        }
        let had = if focus.is_invalid() {
            "no window".to_string()
        } else {
            let mut name = [0u16; 128];
            let n = GetClassNameW(focus, &mut name).max(0) as usize;
            format!("a \"{}\" window", String::from_utf16_lossy(&name[..n]))
        };
        static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.contains(&had) {
            eprintln!("keyboard focus was on {had} while the editor was active; giving it back");
            seen.push(had);
        }
        let _ = SetFocus(Some(hwnd));
    }
    ctx.request_repaint();
}

#[cfg(not(windows))]
pub fn keep(_frame: &eframe::Frame, _ctx: &egui::Context) {}
