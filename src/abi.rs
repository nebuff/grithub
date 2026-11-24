//! The Wasm ABI adapter, written against `docs/APPS.md` (Grimoire 2.3).
//!
//! Only `wasm32` compiles this file, so the rest of the crate unit-tests on the
//! host machine without importing a single symbol.
//!
//! Exports, exactly as the host documents them:
//!
//! ```text
//! app_draw(width, height, scale_milli, elapsed_ms)
//! app_pointer(phase, x, y, source)
//! app_key(code, mods, down)
//! app_status_bar() -> i32
//! app_stop()
//! ```

use std::cell::RefCell;

use crate::app::App;
use crate::host::{Face, Host, HttpRequest, Rect, Style, View};

#[link(wasm_import_module = "grimoire")]
extern "C" {
    fn fill_rect(x: i32, y: i32, w: i32, h: i32, gray: i32);
    /// Unlike the other drawing calls, the host takes `line`'s coordinates and
    /// width as `f32`; `i32` there fails instantiation with a type mismatch.
    fn line(x0: f32, y0: f32, x1: f32, y1: f32, width: f32, gray: i32);
    fn text_at(x: i32, baseline: i32, ptr: *const u8, len: usize, px_milli: i32, face: i32, gray: i32);
    fn text_width(ptr: *const u8, len: usize, px_milli: i32, face: i32) -> i32;
    fn text_height(px_milli: i32, face: i32) -> i32;
    fn card(x: i32, y: i32, w: i32, h: i32, style: i32);
    fn button(
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        label_ptr: *const u8,
        label_len: usize,
        icon_ptr: *const u8,
        icon_len: usize,
        style: i32,
    );

    fn now_ms() -> i64;
    fn request_redraw(after_ms: i32);
    fn close();
    fn state_get(ptr: *mut u8, cap: usize) -> i32;
    fn state_set(ptr: *const u8, len: usize);

    fn http_start(
        method_ptr: *const u8,
        method_len: usize,
        url_ptr: *const u8,
        url_len: usize,
        headers_ptr: *const u8,
        headers_len: usize,
        body_ptr: *const u8,
        body_len: usize,
    ) -> i32;
    fn http_poll() -> i32;
    fn http_status() -> i32;
    fn http_body_len() -> i32;
    fn http_body(ptr: *mut u8, cap: usize) -> i32;
    fn http_error(ptr: *mut u8, cap: usize) -> i32;
}

const MAX_STATE: usize = 256 * 1024;
const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_ERROR: usize = 512;
/// The ABI allows 16 ms to 24 hours.
const MIN_REDRAW_MS: u32 = 16;
const MAX_REDRAW_MS: u32 = 24 * 60 * 60 * 1000;

pub struct WasmHost;

impl Host for WasmHost {
    fn now_ms(&self) -> u64 {
        let ms = unsafe { now_ms() };
        if ms > 0 {
            ms as u64
        } else {
            0
        }
    }

    fn text_width(&self, text: &str, px_milli: i32, face: Face) -> i32 {
        unsafe { text_width(text.as_ptr(), text.len(), px_milli, face.code()) }
    }

    fn text_height(&self, px_milli: i32, face: Face) -> i32 {
        unsafe { text_height(px_milli, face.code()) }
    }

    fn fill_rect(&mut self, r: Rect, gray: u8) {
        unsafe { fill_rect(r.x, r.y, r.w, r.h, gray as i32) }
    }

    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, gray: u8) {
        unsafe {
            line(
                x0 as f32,
                y0 as f32,
                x1 as f32,
                y1 as f32,
                width as f32,
                gray as i32,
            )
        }
    }

    fn text_at(&mut self, x: i32, baseline: i32, text: &str, px_milli: i32, face: Face, gray: u8) {
        unsafe {
            text_at(
                x,
                baseline,
                text.as_ptr(),
                text.len(),
                px_milli,
                face.code(),
                gray as i32,
            )
        }
    }

    fn card(&mut self, r: Rect, style: Style) {
        unsafe { card(r.x, r.y, r.w, r.h, style.code()) }
    }

    fn button(&mut self, r: Rect, label: &str, style: Style) {
        unsafe {
            button(
                r.x,
                r.y,
                r.w,
                r.h,
                label.as_ptr(),
                label.len(),
                std::ptr::null(),
                0,
                style.code(),
            )
        }
    }

    fn http_start(&mut self, req: HttpRequest) -> i32 {
        // The ABI takes one `Name: value` per line, joined by `\n`.
        let headers = req
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");

        unsafe {
            http_start(
                req.method.as_ptr(),
                req.method.len(),
                req.url.as_ptr(),
                req.url.len(),
                headers.as_ptr(),
                headers.len(),
                req.body.as_ptr(),
                req.body.len(),
            )
        }
    }

    fn http_poll(&mut self) -> bool {
        unsafe { http_poll() != 0 }
    }

    fn http_status(&mut self) -> i32 {
        unsafe { http_status() }
    }

    fn http_body(&mut self) -> Vec<u8> {
        let len = unsafe { http_body_len() };
        if len <= 0 {
            return Vec::new();
        }
        let len = (len as usize).min(MAX_BODY);
        let mut buf = vec![0u8; len];
        let written = unsafe { http_body(buf.as_mut_ptr(), len) };
        if written <= 0 {
            return Vec::new();
        }
        buf.truncate((written as usize).min(len));
        buf
    }

    fn http_error(&mut self) -> String {
        let mut buf = vec![0u8; MAX_ERROR];
        let written = unsafe { http_error(buf.as_mut_ptr(), MAX_ERROR) };
        if written <= 0 {
            return String::new();
        }
        buf.truncate((written as usize).min(MAX_ERROR));
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn request_redraw(&mut self, after_ms: u32) {
        let ms = after_ms.clamp(MIN_REDRAW_MS, MAX_REDRAW_MS) as i32;
        unsafe { request_redraw(ms) }
    }

    fn close(&mut self) {
        unsafe { close() }
    }

    fn state_get(&mut self) -> Vec<u8> {
        let mut buf = vec![0u8; MAX_STATE];
        let read = unsafe { state_get(buf.as_mut_ptr(), MAX_STATE) };
        if read <= 0 {
            return Vec::new();
        }
        buf.truncate((read as usize).min(MAX_STATE));
        buf
    }

    fn state_set(&mut self, bytes: &[u8]) {
        let clamped = &bytes[..bytes.len().min(MAX_STATE)];
        unsafe { state_set(clamped.as_ptr(), clamped.len()) };
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App, &mut WasmHost) -> R) -> R {
    APP.with(|cell| {
        let mut slot = cell.borrow_mut();
        let mut host = WasmHost;
        let app = slot.get_or_insert_with(|| App::boot(&mut host));
        f(app, &mut host)
    })
}

#[no_mangle]
pub extern "C" fn app_draw(width: i32, height: i32, scale_milli: i32, _elapsed_ms: i32) {
    with_app(|app, host| {
        app.frame(
            host,
            View {
                w: width,
                h: height,
                scale_milli,
            },
        )
    });
}

/// `phase` is 0 down, 1 move, 2 up; `source` is 0 touch, 1 pen.
#[no_mangle]
pub extern "C" fn app_pointer(phase: i32, x: i32, y: i32, _source: i32) {
    with_app(|app, host| app.on_pointer(host, x, y, phase));
}

#[no_mangle]
pub extern "C" fn app_key(code: i32, _mods: i32, down: i32) {
    if down != 0 {
        with_app(|app, host| app.on_key(host, code));
    }
}

/// 1 starts with Grimoire's shared status bar shown, above the app body.
#[no_mangle]
pub extern "C" fn app_status_bar() -> i32 {
    1
}

#[no_mangle]
pub extern "C" fn app_stop() {
    with_app(|app, host| app.on_stop(host));
}
