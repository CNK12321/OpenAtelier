//! What's dropped onto the window from outside: files, and pictures dragged from a web
//! browser.
//!
//! A browser doesn't hand over a file: dragging a GIF or a picture out of a page gives
//! its bytes as a "virtual file" (Windows' `FileGroupDescriptorW` + `FileContents`),
//! and its address. winit only takes real files (`CF_HDROP`), and refuses the rest at
//! the door, so on Windows the window gets a drop target of its own that takes all
//! three: files as paths, a virtual file as its name and bytes, an address as a link to
//! download. It also says where the pointer is while something's dragged over the
//! window, so the timeline can show where a drop would land. Elsewhere, egui's own
//! dropped files are used.

use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// One thing dropped.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Dropped {
    File(PathBuf),
    /// A file with no place on disk (a picture dragged out of a browser).
    Data { name: String, bytes: Vec<u8> },
    /// An address to download (a `data:` address holds the bytes itself).
    Url(String),
}

#[derive(Default)]
struct Shared {
    /// Where the pointer is while something droppable is over the window (window
    /// pixels).
    hover: Option<[f32; 2]>,
    /// Drops not taken yet, each with where it landed (window pixels).
    dropped: Vec<(Vec<Dropped>, [f32; 2])>,
    ctx: Option<egui::Context>,
}

/// The window's drop target, and what it's been handed.
#[derive(Default)]
pub(crate) struct DropIn {
    shared: Arc<Mutex<Shared>>,
    installed: bool,
}

impl DropIn {
    /// Puts the drop target on the window (once; Windows only — elsewhere egui's own
    /// dropped files are read in [`DropIn::take`]).
    pub(crate) fn install(&mut self, frame: &eframe::Frame, ctx: &egui::Context) {
        if self.installed {
            return;
        }
        self.installed = true;
        self.lock().ctx = Some(ctx.clone());
        #[cfg(windows)]
        if let Err(e) = windows_target::register(frame, self.shared.clone()) {
            eprintln!("drop target: {e}");
        }
        #[cfg(not(windows))]
        let _ = frame;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where something being dragged in from outside is, in points.
    pub(crate) fn hover(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        if cfg!(windows) {
            let ppp = ctx.pixels_per_point();
            self.lock().hover.map(|[x, y]| egui::pos2(x / ppp, y / ppp))
        } else {
            ctx.input(|i| (!i.raw.hovered_files.is_empty()).then(|| i.pointer.latest_pos()).flatten())
        }
    }

    /// Drops since the last call, each with where it landed, in points.
    pub(crate) fn take(&self, ctx: &egui::Context) -> Vec<(Vec<Dropped>, egui::Pos2)> {
        let ppp = ctx.pixels_per_point();
        let mut out: Vec<(Vec<Dropped>, egui::Pos2)> = std::mem::take(&mut self.lock().dropped).into_iter().map(|(d, [x, y])| (d, egui::pos2(x / ppp, y / ppp))).collect();
        // egui's own (every platform; on Windows only if winit's target is in use).
        let (files, at) = ctx.input(|i| {
            let files: Vec<Dropped> = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).map(Dropped::File).collect();
            (files, i.pointer.latest_pos().unwrap_or(egui::Pos2::ZERO))
        });
        if !files.is_empty() {
            out.push((files, at));
        }
        out
    }
}

/// Where dropped pictures and downloads are kept (projects refer to them there).
pub(crate) fn dir() -> PathBuf {
    oa_media::app_dir().join("dropped")
}

/// `name` with a media extension: its own if it has one the editor opens, else what
/// `bytes` look like.
fn with_extension(name: &str, bytes: &[u8]) -> String {
    let path = std::path::Path::new(name);
    let known = path.extension().is_some_and(|e| crate::MEDIA_EXTENSIONS.iter().any(|m| e.eq_ignore_ascii_case(m)));
    match (known, sniff(bytes)) {
        (false, Some(ext)) => format!("{}.{ext}", path.file_stem().map_or_else(|| name.to_string(), |s| s.to_string_lossy().to_string())),
        _ => name.to_string(),
    }
}

/// A free path in `dir` for `name` (`name (2).gif`… if it's taken by another file),
/// or the one already holding these very bytes.
fn unique(dir: &std::path::Path, name: &str, bytes: Option<&[u8]>) -> PathBuf {
    let path = std::path::Path::new(name);
    let (stem, ext) = (path.file_stem().map_or_else(|| name.to_string(), |s| s.to_string_lossy().to_string()), path.extension().map(|e| e.to_string_lossy().to_string()));
    for n in 1.. {
        let candidate = match (n, &ext) {
            (1, _) => dir.join(name),
            (_, Some(e)) => dir.join(format!("{stem} ({n}).{e}")),
            (_, None) => dir.join(format!("{stem} ({n})")),
        };
        let same = bytes.is_some_and(|b| std::fs::read(&candidate).is_ok_and(|have| have == b));
        if same || !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("some name is free")
}

/// Saves a dropped picture (bytes from a browser) where the editor keeps them.
pub(crate) fn save(name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = unique(&dir, &with_extension(&safe_name(name), bytes), Some(bytes));
    if !path.exists() {
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// Downloads `url` where dropped pictures are kept (with curl, as the updater does),
/// named after it — and given the extension its contents say, if it has none.
pub(crate) fn download(url: &str) -> Result<PathBuf, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("can't download {url}"));
    }
    let dir = dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = url.split(['?', '#']).next().unwrap_or(url).rsplit('/').find(|s| !s.is_empty()).unwrap_or("download");
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let part = dir.join(format!(".download-{}-{n}", std::process::id()));
    let out = oa_media::tool("curl")
        .args(["-sS", "-L", "--fail", "--max-time", "120", "--max-filesize", "2000000000", "-A", "Mozilla/5.0 OpenAtelier", "-o"])
        .arg(&part)
        .arg(url)
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("couldn't download {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let head = std::fs::read(&part).map(|b| b[..b.len().min(64)].to_vec()).unwrap_or_default();
    if head.starts_with(b"<") || head.starts_with(b"\xEF\xBB\xBF<") {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{url} is a web page, not a picture or a video"));
    }
    let path = unique(&dir, &with_extension(&safe_name(name), &head), None);
    std::fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// A file name that's safe to save under: no folders, nothing Windows refuses, not
/// too long.
pub(crate) fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut out: String = base.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    out = out.trim().trim_matches('.').to_string();
    if out.chars().count() > 120 {
        let ext = std::path::Path::new(&out).extension().map(|e| e.to_string_lossy().to_string());
        let stem: String = out.chars().take(100).collect();
        out = match ext {
            Some(e) if e.len() <= 8 => format!("{stem}.{e}"),
            _ => stem,
        };
    }
    if out.is_empty() { "dropped".into() } else { out }
}

/// The file type `bytes` start like, as an extension, for a file that came without one.
pub(crate) fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let at = |i: usize, s: &[u8]| bytes.get(i..i + s.len()) == Some(s);
    if at(0, b"GIF8") {
        Some("gif")
    } else if at(0, b"\x89PNG") {
        Some("png")
    } else if at(0, b"\xFF\xD8\xFF") {
        Some("jpg")
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        Some("webp")
    } else if at(4, b"ftyp") {
        Some("mp4")
    } else if at(0, b"\x1A\x45\xDF\xA3") {
        Some("webm")
    } else if at(0, b"BM") {
        Some("bmp")
    } else if at(0, b"ID3") || at(0, b"\xFF\xFB") {
        Some("mp3")
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        Some("wav")
    } else {
        None
    }
}

/// The bytes in a `data:` address (`data:image/gif;base64,…`), if it is one.
pub(crate) fn data_url(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("data:")?;
    let (head, body) = rest.split_once(',')?;
    if !head.ends_with(";base64") {
        return Some(body.as_bytes().to_vec());
    }
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in body.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        acc = (acc << 6) | value(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(windows)]
mod windows_target {
    use super::{Dropped, Shared};
    use std::sync::{Arc, Mutex};
    use windows::core::{implement, w, Ref, Result};
    use windows::Win32::Foundation::{HWND, POINT, POINTL};
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL, TYMED_ISTREAM};
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::{IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE};
    use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
    use windows::Win32::UI::Shell::{DragQueryFileW, FILEDESCRIPTORW, FILEGROUPDESCRIPTORW, HDROP};

    pub(super) fn register(frame: &eframe::Frame, shared: Arc<Mutex<Shared>>) -> std::result::Result<(), String> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = frame.window_handle().map_err(|e| e.to_string())?;
        let RawWindowHandle::Win32(w) = handle.as_raw() else { return Err("not a Win32 window".into()) };
        let hwnd = HWND(w.hwnd.get() as *mut _);
        let target: IDropTarget = Target { hwnd, shared, accept: Mutex::new(false) }.into();
        // SAFETY: OLE on the window's own thread; winit's target (if any) is replaced.
        unsafe {
            let _ = OleInitialize(None);
            let _ = RevokeDragDrop(hwnd);
            RegisterDragDrop(hwnd, &target).map_err(|e| e.to_string())
        }
    }

    #[implement(IDropTarget)]
    struct Target {
        hwnd: HWND,
        shared: Arc<Mutex<Shared>>,
        /// What's over the window now is something we take.
        accept: Mutex<bool>,
    }

    impl Target {
        fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
            self.shared.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// The pointer in window pixels.
        fn client(&self, pt: &POINTL) -> [f32; 2] {
            let mut p = POINT { x: pt.x, y: pt.y };
            // SAFETY: our own window.
            unsafe {
                let _ = ScreenToClient(self.hwnd, &mut p);
            }
            [p.x as f32, p.y as f32]
        }

        fn hover(&self, at: Option<[f32; 2]>) {
            let mut s = self.shared();
            s.hover = at;
            if let Some(ctx) = &s.ctx {
                ctx.request_repaint();
            }
        }

        fn effect(&self, effect: *mut DROPEFFECT) {
            let accept = *self.accept.lock().unwrap_or_else(|e| e.into_inner());
            if !effect.is_null() {
                // SAFETY: the effect OLE hands us to write.
                unsafe { *effect = if accept { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
            }
        }
    }

    impl IDropTarget_Impl for Target_Impl {
        fn DragEnter(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            let accept = data.as_ref().is_some_and(offers);
            *self.accept.lock().unwrap_or_else(|e| e.into_inner()) = accept;
            self.hover(accept.then(|| self.client(pt)));
            self.effect(effect);
            Ok(())
        }

        fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            let accept = *self.accept.lock().unwrap_or_else(|e| e.into_inner());
            self.hover(accept.then(|| self.client(pt)));
            self.effect(effect);
            Ok(())
        }

        fn DragLeave(&self) -> Result<()> {
            self.hover(None);
            Ok(())
        }

        fn Drop(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            let items = data.as_ref().map(read).unwrap_or_default();
            *self.accept.lock().unwrap_or_else(|e| e.into_inner()) = !items.is_empty();
            self.effect(effect);
            let at = self.client(pt);
            let mut s = self.shared();
            s.hover = None;
            if !items.is_empty() {
                s.dropped.push((items, at));
            }
            if let Some(ctx) = &s.ctx {
                ctx.request_repaint();
            }
            Ok(())
        }
    }

    fn format(cf: u16, tymed: i32, index: i32) -> FORMATETC {
        FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: index, tymed: tymed as u32 }
    }

    fn registered(name: windows::core::PCWSTR) -> u16 {
        // SAFETY: a constant string.
        unsafe { RegisterClipboardFormatW(name) as u16 }
    }

    /// Whether the data holds anything we take: files, a virtual file or an address.
    fn offers(data: &IDataObject) -> bool {
        let has = |cf: u16| {
            // SAFETY: a query on the data object OLE handed us.
            unsafe { data.QueryGetData(&format(cf, TYMED_HGLOBAL.0, -1)).is_ok() }
        };
        has(CF_HDROP.0) || has(registered(w!("FileGroupDescriptorW"))) || has(registered(w!("UniformResourceLocatorW"))) || text_url(data).is_some()
    }

    /// The bytes of an HGLOBAL medium.
    fn global_bytes(medium: &STGMEDIUM) -> Vec<u8> {
        // SAFETY: the medium's HGLOBAL, locked while it's copied.
        unsafe {
            let h = medium.u.hGlobal;
            let size = GlobalSize(h);
            let ptr = GlobalLock(h) as *const u8;
            if ptr.is_null() {
                return Vec::new();
            }
            let out = std::slice::from_raw_parts(ptr, size).to_vec();
            let _ = GlobalUnlock(h);
            out
        }
    }

    /// A medium's data as UTF-16 text, up to its end or a nul.
    fn utf16(bytes: &[u8]) -> String {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|u| *u != 0).collect();
        String::from_utf16_lossy(&units)
    }

    fn get(data: &IDataObject, f: &FORMATETC) -> Option<STGMEDIUM> {
        // SAFETY: asks the data object for a format it said it has.
        unsafe { data.GetData(f).ok() }
    }

    fn release(mut medium: STGMEDIUM) {
        // SAFETY: a medium GetData gave us, released once.
        unsafe { ReleaseStgMedium(&mut medium) };
    }

    /// Plain text that's an address (a link dragged as text).
    fn text_url(data: &IDataObject) -> Option<String> {
        let medium = get(data, &format(CF_UNICODETEXT.0, TYMED_HGLOBAL.0, -1))?;
        let text = utf16(&global_bytes(&medium)).trim().to_string();
        release(medium);
        (text.starts_with("http://") || text.starts_with("https://") || text.starts_with("data:")).then_some(text)
    }

    /// Everything we take out of the data: files first; else virtual files; else an
    /// address.
    fn read(data: &IDataObject) -> Vec<Dropped> {
        let mut out = Vec::new();
        if let Some(medium) = get(data, &format(CF_HDROP.0, TYMED_HGLOBAL.0, -1)) {
            // SAFETY: the HDROP in the medium, read before it's released.
            unsafe {
                let drop = HDROP(medium.u.hGlobal.0);
                for i in 0..DragQueryFileW(drop, u32::MAX, None) {
                    let len = DragQueryFileW(drop, i, None) as usize;
                    let mut name = vec![0u16; len + 1];
                    DragQueryFileW(drop, i, Some(&mut name));
                    out.push(Dropped::File(String::from_utf16_lossy(&name[..len]).into()));
                }
            }
            release(medium);
            if !out.is_empty() {
                return out;
            }
        }
        out.extend(virtual_files(data));
        if !out.is_empty() {
            return out;
        }
        let url = get(data, &format(registered(w!("UniformResourceLocatorW")), TYMED_HGLOBAL.0, -1)).map(|m| {
            let url = utf16(&global_bytes(&m));
            release(m);
            url
        });
        if let Some(url) = url.filter(|u| !u.is_empty()).or_else(|| text_url(data)) {
            out.push(Dropped::Url(url.trim().to_string()));
        }
        out
    }

    /// Files with no place on disk: their names from the descriptor, their bytes from
    /// `FileContents` (a block of memory or a stream).
    fn virtual_files(data: &IDataObject) -> Vec<Dropped> {
        let Some(medium) = get(data, &format(registered(w!("FileGroupDescriptorW")), TYMED_HGLOBAL.0, -1)) else { return Vec::new() };
        let bytes = global_bytes(&medium);
        release(medium);
        if bytes.len() < 4 {
            return Vec::new();
        }
        let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let first = std::mem::offset_of!(FILEGROUPDESCRIPTORW, fgd);
        let size = std::mem::size_of::<FILEDESCRIPTORW>();
        let contents = registered(w!("FileContents"));
        let mut out = Vec::new();
        for i in 0..count.min(64) {
            let at = first + i * size;
            let Some(raw) = bytes.get(at..at + size) else { break };
            // SAFETY: a FILEDESCRIPTORW's bytes, read unaligned.
            let d: FILEDESCRIPTORW = unsafe { std::ptr::read_unaligned(raw.as_ptr() as *const FILEDESCRIPTORW) };
            // (Copied out: the descriptor is packed.)
            let raw_name = d.cFileName;
            let name = String::from_utf16_lossy(&raw_name[..raw_name.iter().position(|c| *c == 0).unwrap_or(260)]);
            let Some(medium) = get(data, &format(contents, TYMED_HGLOBAL.0 | TYMED_ISTREAM.0, i as i32)) else { continue };
            let mut file = if medium.tymed == TYMED_ISTREAM.0 as u32 {
                // SAFETY: the stream in the medium, read to its end before release.
                unsafe { (*medium.u.pstm).as_ref().map(read_stream).unwrap_or_default() }
            } else {
                global_bytes(&medium)
            };
            release(medium);
            // A block of memory can be bigger than the file: the descriptor says how big.
            let declared = ((d.nFileSizeHigh as u64) << 32) | d.nFileSizeLow as u64;
            if declared > 0 && (declared as usize) < file.len() {
                file.truncate(declared as usize);
            }
            if !file.is_empty() {
                out.push(Dropped::Data { name, bytes: file });
            }
        }
        out
    }

    fn read_stream(stream: &windows::Win32::System::Com::IStream) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let mut got = 0u32;
            // SAFETY: reads into our buffer, no more than its length.
            let hr = unsafe { stream.Read(buf.as_mut_ptr() as *mut _, buf.len() as u32, Some(&mut got)) };
            if hr.is_err() || got == 0 {
                break;
            }
            out.extend_from_slice(&buf[..got as usize]);
            if out.len() > 2 << 30 {
                break;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_addresses_decode() {
        assert_eq!(data_url("data:image/gif;base64,R0lGODlh").as_deref(), Some(&b"GIF89a"[..]));
        assert_eq!(data_url("data:text/plain,hi").as_deref(), Some(&b"hi"[..]));
        assert_eq!(data_url("https://example.com/a.gif"), None);
    }

    #[test]
    fn files_without_an_extension_are_recognized() {
        assert_eq!(sniff(b"GIF89a\x01\x00"), Some("gif"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n"), Some("png"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn dropped_names_are_safe_to_save() {
        assert_eq!(safe_name("C:\\x\\cat?.gif"), "cat_.gif");
        assert_eq!(safe_name("../.."), "dropped");
        assert_eq!(safe_name("a<b>:c.png"), "a_b__c.png");
    }
}
