//! The app's contract with Grimoire, taken from `docs/APPS.md` (2.3).
//!
//! Everything above this module is plain Rust that unit-tests on the host.
//! Everything below mirrors the documented `grimoire` ABI; `abi.rs` is the only
//! file that turns these calls into imports.

/// A rectangle in screen pixels, origin at the top-left of the app body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.right() && py < self.bottom()
    }

    pub fn inset(&self, d: i32) -> Self {
        Self::new(
            self.x + d,
            self.y + d,
            (self.w - 2 * d).max(0),
            (self.h - 2 * d).max(0),
        )
    }
}

/// The body geometry handed to `app_draw`; `scale_milli` is 1000 at the normal
/// UI scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    pub w: i32,
    pub h: i32,
    pub scale_milli: i32,
}

impl Default for View {
    fn default() -> Self {
        Self {
            w: 1860,
            h: 2480,
            scale_milli: 1000,
        }
    }
}

/// Text face codes: 0 regular, 1 bold, 2 title, 3 caps, 4 fixed, 5 fixed bold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Regular,
    Bold,
    Title,
    Caps,
    Fixed,
    FixedBold,
}

impl Face {
    pub fn code(self) -> i32 {
        match self {
            Face::Regular => 0,
            Face::Bold => 1,
            Face::Title => 2,
            Face::Caps => 3,
            Face::Fixed => 4,
            Face::FixedBold => 5,
        }
    }
}

/// Themed element styles: 0 plain, 1 selected, 2 disabled, 3 primary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    Selected,
    Disabled,
    Primary,
}

impl Style {
    pub fn code(self) -> i32 {
        match self {
            Style::Plain => 0,
            Style::Selected => 1,
            Style::Disabled => 2,
            Style::Primary => 3,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    /// One `Name: value` per entry; the adapter joins them with `\n`.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The host exposes one request at a time, so no handle is involved.
pub trait Host {
    /// `now_ms` is i64 and monotonic on the device.
    fn now_ms(&self) -> u64;

    fn text_width(&self, text: &str, px_milli: i32, face: Face) -> i32;

    fn text_height(&self, px_milli: i32, face: Face) -> i32;

    /// Gray is 0 (black) to 255 (paper white).
    fn fill_rect(&mut self, r: Rect, gray: u8);

    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, gray: u8);

    /// `baseline` is the text baseline, not the top edge.
    fn text_at(&mut self, x: i32, baseline: i32, text: &str, px_milli: i32, face: Face, gray: u8);

    fn card(&mut self, r: Rect, style: Style);

    fn button(&mut self, r: Rect, label: &str, style: Style);

    /// 0 started, -1 bad argument, -2 missing `network:http`, -3 already busy.
    fn http_start(&mut self, req: HttpRequest) -> i32;

    /// False while the request runs, true once it has finished.
    fn http_poll(&mut self) -> bool;

    /// The HTTP status, or -1 when the request failed without one.
    fn http_status(&mut self) -> i32;

    fn http_body(&mut self) -> Vec<u8>;

    /// Curl's one-line failure when `http_status` is -1.
    fn http_error(&mut self) -> String;

    /// The app's only timer: 16 ms to 24 hours.
    fn request_redraw(&mut self, after_ms: u32);

    /// Asks the host to run `app_stop`, save and return to the Library.
    fn close(&mut self);

    fn state_get(&mut self) -> Vec<u8>;

    fn state_set(&mut self, bytes: &[u8]);
}

/// Test double: records drawn labels and scheduled frames, serves canned HTTP.
#[cfg(test)]
pub mod mock {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Clone)]
    struct Resp {
        status: i32,
        body: Vec<u8>,
        error: String,
        polls: u32,
        start_error: i32,
    }

    pub struct MockHost {
        pub now: u64,
        pub state: Vec<u8>,
        pub requests: Vec<HttpRequest>,
        pub draws: Vec<String>,
        pub redraws: Vec<u32>,
        pub closed: bool,
        pending: VecDeque<Resp>,
        active: Option<Resp>,
    }

    impl MockHost {
        pub fn new() -> Self {
            Self {
                now: 1_000,
                state: Vec::new(),
                requests: Vec::new(),
                draws: Vec::new(),
                redraws: Vec::new(),
                closed: false,
                pending: VecDeque::new(),
                active: None,
            }
        }

        pub fn push(&mut self, status: i32, body: &str) {
            self.pending.push_back(Resp {
                status,
                body: body.as_bytes().to_vec(),
                error: String::new(),
                polls: 0,
                start_error: 0,
            });
        }

        pub fn push_failure(&mut self, error: &str) {
            self.pending.push_back(Resp {
                status: -1,
                body: Vec::new(),
                error: error.to_owned(),
                polls: 0,
                start_error: 0,
            });
        }

        /// Make the next response stay pending for `polls` calls to http_poll.
        pub fn stall_next(&mut self, polls: u32) {
            if let Some(last) = self.pending.back_mut() {
                last.polls = polls;
            }
        }

        pub fn push_start_error(&mut self, code: i32) {
            self.pending.push_back(Resp {
                status: -1,
                body: Vec::new(),
                error: String::new(),
                polls: 0,
                start_error: code,
            });
        }

        pub fn drew(&self, needle: &str) -> bool {
            self.draws.iter().any(|d| d.contains(needle))
        }

        pub fn last_redraw(&self) -> Option<u32> {
            self.redraws.last().copied()
        }
    }

    impl Host for MockHost {
        fn now_ms(&self) -> u64 {
            self.now
        }

        fn text_width(&self, text: &str, px_milli: i32, _face: Face) -> i32 {
            let px = (px_milli / 1000).max(1);
            (text.chars().count() as i32).max(1) * px * 6 / 10
        }

        fn text_height(&self, px_milli: i32, _face: Face) -> i32 {
            (px_milli / 1000).max(1)
        }

        fn fill_rect(&mut self, r: Rect, _gray: u8) {
            self.draws.push(format!("fill:{}x{}", r.w, r.h));
        }

        fn line(&mut self, _x0: i32, _y0: i32, _x1: i32, _y1: i32, _w: i32, _gray: u8) {
            self.draws.push("line".to_owned());
        }

        fn text_at(&mut self, _x: i32, _baseline: i32, text: &str, _px: i32, _face: Face, _g: u8) {
            self.draws.push(format!("text:{text}"));
        }

        fn card(&mut self, r: Rect, _style: Style) {
            self.draws.push(format!("card:{}x{}", r.w, r.h));
        }

        fn button(&mut self, _r: Rect, label: &str, _style: Style) {
            self.draws.push(format!("button:{label}"));
        }

        fn http_start(&mut self, req: HttpRequest) -> i32 {
            self.requests.push(req);
            match self.pending.pop_front() {
                Some(r) if r.start_error != 0 => {
                    self.active = None;
                    r.start_error
                }
                Some(r) => {
                    self.active = Some(r);
                    0
                }
                None => {
                    self.active = None;
                    -1
                }
            }
        }

        fn http_poll(&mut self) -> bool {
            match self.active.as_mut() {
                None => true,
                Some(r) if r.polls > 0 => {
                    r.polls -= 1;
                    false
                }
                Some(_) => true,
            }
        }

        fn http_status(&mut self) -> i32 {
            self.active.as_ref().map(|r| r.status).unwrap_or(-1)
        }

        fn http_body(&mut self) -> Vec<u8> {
            self.active.as_ref().map(|r| r.body.clone()).unwrap_or_default()
        }

        fn http_error(&mut self) -> String {
            self.active.as_ref().map(|r| r.error.clone()).unwrap_or_default()
        }

        fn request_redraw(&mut self, after_ms: u32) {
            self.redraws.push(after_ms);
        }

        fn close(&mut self) {
            self.closed = true;
        }

        fn state_get(&mut self) -> Vec<u8> {
            self.state.clone()
        }

        fn state_set(&mut self, bytes: &[u8]) {
            self.state = bytes.to_vec();
        }
    }
}
