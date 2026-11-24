//! Pure geometry and text layout. No host calls, no side effects: everything in
//! here is exercised by unit tests rather than on a device.
//!
//! Sizes derive from the body dimensions and `scale_milli` that `app_draw`
//! provides, so the app follows the owner's UI scale instead of assuming the
//! Scribe's 1860x2480 panel.

use crate::host::Rect;

/// `px_milli` values are thousandths of a pixel, as the ABI's text calls want.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub pad: i32,
    pub header_h: i32,
    pub footer_h: i32,
    pub gap: i32,
    pub col_header_h: i32,
    pub line_h: i32,
    pub card_pad: i32,
    pub body_px: i32,
    pub head_px: i32,
    pub big_px: i32,
    pub small_px: i32,
    pub button_h: i32,
}

/// The sizes are Grimoire's own, in the 300 dpi pixels its UI is laid out in
/// (body text 44, title 64, a header 150), multiplied by `scale_milli` — 1000 at
/// the normal UI scale — so the app follows the owner's Settings > Display
/// choice instead of fixing a size of its own.
impl Metrics {
    pub fn new(_w: i32, _h: i32, scale_milli: i32) -> Self {
        let s = scale_milli.clamp(500, 3000);
        let at = |nominal: i32| (nominal * s / 1000).max(1);

        let body = at(44);
        let body_px = body * 1000;
        let button_h = at(120);

        Self {
            pad: at(50),
            header_h: at(150),
            footer_h: button_h + at(50),
            gap: at(20),
            col_header_h: at(80),
            line_h: body * 3 / 2,
            card_pad: at(22),
            body_px,
            head_px: at(64) * 1000,
            big_px: at(96) * 1000,
            small_px: at(34) * 1000,
            button_h,
        }
    }
}

/// How many columns fit side by side. Portrait pages one column at a time:
/// squeezing five readable columns out of an 1860 px width is worse than paging.
pub fn columns_across(m: &Metrics, w: i32, landscape: bool) -> usize {
    if !landscape {
        return 1;
    }
    let usable = (w - 2 * m.pad).max(1);
    let min_width = (m.pad * 8).max(1);
    ((usable / min_width).clamp(1, 6)) as usize
}

pub fn page_count(columns: usize, per_page: usize) -> usize {
    let per = per_page.max(1);
    ((columns + per - 1) / per).max(1)
}

pub fn column_rect(m: &Metrics, w: i32, h: i32, index: usize, per_page: usize) -> Rect {
    let per = per_page.max(1) as i32;
    let usable = (w - 2 * m.pad).max(1);
    let width = (usable - (per - 1) * m.gap) / per;
    let x = m.pad + index as i32 * (width + m.gap);
    let y = m.header_h + m.pad / 2;
    let bottom = h - m.footer_h;
    Rect::new(x, y, width.max(1), (bottom - y).max(1))
}

/// Greedy word wrap using the host's own measurement, so text never overflows
/// a card on a theme with a wider font.
pub fn wrap(
    text: &str,
    max_w: i32,
    max_lines: usize,
    measure: &dyn Fn(&str) -> i32,
) -> Vec<String> {
    let max_lines = max_lines.max(1);
    let max_w = max_w.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut overflow = false;

    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };

        if measure(&candidate) <= max_w {
            line = candidate;
            continue;
        }

        if line.is_empty() {
            // A single word wider than the card: break it on characters.
            let mut head = String::new();
            let mut rest = false;
            for ch in word.chars() {
                if rest {
                    continue;
                }
                let mut trial = head.clone();
                trial.push(ch);
                if measure(&trial) <= max_w {
                    head = trial;
                } else {
                    rest = true;
                }
            }
            line = head;
            if rest {
                overflow = true;
            }
            continue;
        }

        out.push(std::mem::take(&mut line));
        if out.len() >= max_lines {
            overflow = true;
            break;
        }
        line = word.to_owned();
    }

    if !line.is_empty() {
        if out.len() < max_lines {
            out.push(line);
        } else {
            overflow = true;
        }
    }

    if overflow {
        match out.last_mut() {
            Some(last) => *last = ellipsize(last, max_w, measure),
            None => out.push("…".to_owned()),
        }
    }

    out
}

fn ellipsize(line: &str, max_w: i32, measure: &dyn Fn(&str) -> i32) -> String {
    let mut chars: Vec<char> = line.chars().collect();
    loop {
        let mut candidate: String = chars.iter().collect();
        candidate.push('…');
        if measure(&candidate) <= max_w || chars.is_empty() {
            return candidate;
        }
        chars.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic stand-in for the host: 10 px per character.
    fn measure(text: &str) -> i32 {
        text.chars().count() as i32 * 10
    }

    #[test]
    fn metrics_follow_the_ui_scale() {
        let normal = Metrics::new(2480, 1860, 1000);
        let large = Metrics::new(2480, 1860, 1500);

        assert_eq!(normal.body_px, 44_000);
        assert_eq!(large.body_px, 66_000);
        assert!(large.line_h > normal.line_h);
        assert_eq!(normal.pad, 50);
        assert_eq!(normal.button_h, 120);
    }

    #[test]
    fn landscape_fits_five_columns_and_portrait_fits_one() {
        let m = Metrics::new(2480, 1860, 1000);
        assert_eq!(columns_across(&m, 2480, true), 5);

        let p = Metrics::new(1860, 2480, 1000);
        assert_eq!(columns_across(&p, 1860, false), 1);
    }

    #[test]
    fn columns_tile_the_screen_without_overlapping_or_overflowing() {
        let (w, h) = (2480, 1860);
        let m = Metrics::new(w, h, 1000);
        let per = columns_across(&m, w, true);
        let rects: Vec<Rect> = (0..per).map(|i| column_rect(&m, w, h, i, per)).collect();

        assert_eq!(rects[0].x, m.pad);
        assert!(rects.last().unwrap().right() <= w - m.pad);
        for pair in rects.windows(2) {
            assert_eq!(pair[1].x, pair[0].right() + m.gap);
        }
        assert!(rects[0].bottom() <= h - m.footer_h);
    }

    #[test]
    fn page_count_rounds_up_and_never_returns_zero() {
        assert_eq!(page_count(0, 5), 1);
        assert_eq!(page_count(5, 5), 1);
        assert_eq!(page_count(6, 5), 2);
    }

    #[test]
    fn wrap_breaks_on_words_and_respects_the_line_cap() {
        let lines = wrap("alpha beta gamma delta", 100, 3, &measure);
        assert_eq!(lines, vec!["alpha beta", "gamma", "delta"]);
    }

    #[test]
    fn wrap_marks_overflow_on_the_last_visible_line() {
        let lines = wrap("one two three four five", 60, 2, &measure);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
    }

    #[test]
    fn wrap_hard_breaks_a_word_that_cannot_fit() {
        let lines = wrap("supercalifragilistic", 80, 3, &measure);
        assert!(measure(&lines[0]) <= 80);
        assert!(lines[0].ends_with('…'));
    }

    #[test]
    fn wrap_handles_empty_text() {
        assert!(wrap("   ", 100, 2, &measure).is_empty());
    }
}
