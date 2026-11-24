//! Screen drawing against the documented ABI: primitives take a grayscale
//! value, themed elements take a style, and `text_at` takes a baseline.
//!
//! Layout is recomputed per paint rather than retained: at these sizes it costs
//! nothing, and it removes a whole class of stale-rectangle bugs.
//!
//! Hit regions are pushed by the same function that draws, so a key can never
//! be drawn somewhere the user cannot tap.

use crate::app::Hit;
use crate::auth::Phase;
use crate::host::{Face, Host, Rect, Style, View};
use crate::layout::{self, Metrics};
use crate::model::{Board, Card, Column, ProjectRef};

/// Grays the app chooses for its own primitives. Themed elements (`card`,
/// `button`) take a style instead, so the owner's theme still decides their
/// frame and fill; these are only for paper, text and rules.
pub const PAPER: u8 = 255;
pub const INK: u8 = 0;
pub const MUTED: u8 = 110;
pub const RULE: u8 = 190;

pub struct SetupView<'a> {
    pub input: &'a str,
    pub shift: bool,
    pub error: Option<&'a str>,
    /// True when a client id is already stored, which makes Cancel meaningful.
    pub existing: bool,
}

pub struct AuthView<'a> {
    pub phase: &'a Phase,
    pub user_code: Option<&'a str>,
    pub uri: Option<&'a str>,
    pub status: Option<&'a str>,
    pub diagnostics: Option<&'a str>,
    pub client_id: Option<&'a str>,
}

pub struct PickerView<'a> {
    pub projects: &'a [ProjectRef],
    pub login: Option<&'a str>,
    pub status: Option<&'a str>,
    pub syncing: bool,
}

pub struct BoardView<'a> {
    pub board: &'a Board,
    pub page: usize,
    pub login: Option<&'a str>,
    pub status: Option<&'a str>,
    pub syncing: bool,
}

pub struct DetailView<'a> {
    pub card: &'a Card,
    pub column: &'a str,
    pub project: &'a str,
    pub field_name: &'a str,
}

/// `text_at` positions the baseline, so every call site converts its top edge.
fn baseline<H: Host>(host: &H, top: i32, px: i32, face: Face) -> i32 {
    top + host.text_height(px, face)
}

/// Single-line clipping with an ellipsis, measured by the host's own font.
fn fit<H: Host>(host: &H, text: &str, max_w: i32, px: i32, face: Face) -> String {
    if host.text_width(text, px, face) <= max_w {
        return text.to_owned();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let mut candidate: String = chars.iter().collect();
        candidate.push('…');
        if host.text_width(&candidate, px, face) <= max_w {
            return candidate;
        }
    }
    "…".to_owned()
}

/// Clip a long value from the left: while typing, the tail is the part that
/// matters, and a client id's prefix is the boilerplate.
fn fit_tail<H: Host>(host: &H, text: &str, max_w: i32, px: i32, face: Face) -> String {
    if host.text_width(text, px, face) <= max_w {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        let candidate: String = std::iter::once('…').chain(chars[start..].iter().copied()).collect();
        if host.text_width(&candidate, px, face) <= max_w {
            return candidate;
        }
    }
    "…".to_owned()
}

fn muted_line<H: Host>(host: &mut H, x: i32, top: i32, px: i32, text: &str) {
    let t = baseline(host, top, px, Face::Regular);
    host.text_at(x, t, text, px, Face::Regular, MUTED);
}

/// The ✕ at the top right of every screen. Grimoire's own header action buttons
/// are 96 px squares; a 120 px target is easier to hit with a finger and still
/// clears the title.
fn close_button<H: Host>(host: &mut H, m: &Metrics, view: View, hits: &mut Vec<(Rect, Hit)>) {
    let d = m.button_h;
    let rect = Rect::new(view.w - m.pad - d, (m.header_h - d) / 2, d, d);
    host.button(rect, "✕", Style::Plain);
    hits.push((rect, Hit::Close));
}

/// A key drawn with the host's own button, so the keyboard is Grimoire's
/// rounded, themed control at Grimoire's size instead of a lookalike.
fn setup_key<H: Host>(
    host: &mut H,
    rect: Rect,
    label: &str,
    style: Style,
    hit: Hit,
    hits: &mut Vec<(Rect, Hit)>,
) {
    host.button(rect, label, style);
    hits.push((rect, hit));
}

/// First launch: the client id is typed here, not compiled in. The keyboard is
/// the app's own because the ABI exposes no text input.
pub fn draw_setup<H: Host>(
    host: &mut H,
    v: SetupView<'_>,
    view: View,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let m = Metrics::new(view.w, view.h, view.scale_milli);
    host.fill_rect(Rect::new(0, 0, view.w, view.h), PAPER);

    let left = m.pad * 2;
    let width = (view.w - left * 2).max(1);
    let mut y = m.pad;

    let t = baseline(host, y, m.head_px, Face::Title);
    host.text_at(left, t, "Set up GitHub access", m.head_px, Face::Title, INK);
    y += m.line_h * 2;

    muted_line(
        host,
        left,
        y,
        m.body_px,
        "grithub signs in through a GitHub OAuth App that you create once:",
    );
    muted_line(
        host,
        left,
        y + m.line_h,
        m.body_px,
        "github.com/settings/developers → New OAuth App → tick “Enable Device Flow”.",
    );
    muted_line(
        host,
        left,
        y + m.line_h * 2,
        m.body_px,
        "The client ID below is public. The token it returns stays on this device.",
    );
    y += m.line_h * 3 + m.pad;

    let t = baseline(host, y, m.body_px, Face::Bold);
    host.text_at(left, t, "OAuth App Client ID", m.body_px, Face::Bold, INK);
    y += m.line_h + m.gap;

    let field = Rect::new(left, y, width, m.button_h + m.card_pad);
    host.card(field, Style::Selected);

    let face = Face::Fixed;
    let px = m.head_px;
    let inner = (field.w - 2 * m.card_pad - m.line_h).max(1);
    let shown = if v.input.is_empty() {
        "Iv1.0123abcd…".to_owned()
    } else {
        fit_tail(host, v.input, inner, px, face)
    };
    let ink = if v.input.is_empty() { MUTED } else { INK };
    let th = host.text_height(px, face);
    let text_y = field.y + (field.h - th) / 2 + th;
    host.text_at(field.x + m.card_pad, text_y, &shown, px, face, ink);

    if !v.input.is_empty() {
        let tw = host.text_width(&shown, px, face);
        let caret_x = field.x + m.card_pad + tw + m.gap;
        if caret_x < field.right() - m.card_pad {
            host.line(
                caret_x,
                field.y + m.card_pad,
                caret_x,
                field.bottom() - m.card_pad,
                2,
                INK,
            );
        }
    }
    y = field.bottom() + m.gap;

    match v.error {
        Some(err) => {
            let t = baseline(host, y, m.body_px, Face::Bold);
            host.text_at(left, t, err, m.body_px, Face::Bold, INK);
        }
        None => muted_line(
            host,
            left,
            y,
            m.body_px,
            "Stored in this app's state. Tap Continue to sign in.",
        ),
    }

    // ---- keyboard ----
    let kb_bottom = view.h - m.footer_h - m.pad;
    let kb_top_limit = y + m.line_h + m.pad;
    let kb_h = (kb_bottom - kb_top_limit)
        .clamp(0, (view.h * 45 / 100).max(1))
        .max(0);
    let gap = m.gap;
    let unit = (view.w - 2 * m.pad - 9 * gap) / 10;
    let key_h = (kb_h - 4 * gap) / 5;
    let kb_top = kb_bottom - kb_h;
    if key_h > 0 {
        // QWERTYUIOP
        for (i, ch) in "qwertyuiop".chars().enumerate() {
            let x = m.pad + i as i32 * (unit + gap);
            let label = ch.to_ascii_uppercase().to_string();
            setup_key(
                host,
                Rect::new(x, kb_top, unit, key_h),
                &label,
                Style::Plain,
                Hit::SetupChar(ch as u8),
                hits,
            );
        }

        // ASDFGHJKL, centred.
        let row_w = 9 * unit + 8 * gap;
        let row_x = (view.w - row_w) / 2;
        let y1 = kb_top + key_h + gap;
        for (i, ch) in "asdfghjkl".chars().enumerate() {
            let x = row_x + i as i32 * (unit + gap);
            let label = ch.to_ascii_uppercase().to_string();
            setup_key(
                host,
                Rect::new(x, y1, unit, key_h),
                &label,
                Style::Plain,
                Hit::SetupChar(ch as u8),
                hits,
            );
        }

        // SHIFT ZXCVBNM DEL
        let y2 = kb_top + (key_h + gap) * 2;
        setup_key(
            host,
            Rect::new(m.pad, y2, unit, key_h),
            "SHIFT",
            if v.shift { Style::Selected } else { Style::Plain },
            Hit::SetupShift,
            hits,
        );
        for (i, ch) in "zxcvbnm".chars().enumerate() {
            let x = m.pad + (i as i32 + 1) * (unit + gap);
            let label = ch.to_ascii_uppercase().to_string();
            setup_key(
                host,
                Rect::new(x, y2, unit, key_h),
                &label,
                Style::Plain,
                Hit::SetupChar(ch as u8),
                hits,
            );
        }
        setup_key(
            host,
            Rect::new(m.pad + 9 * (unit + gap), y2, unit, key_h),
            "DEL",
            Style::Plain,
            Hit::SetupBackspace,
            hits,
        );

        // 1234567890
        let y3 = kb_top + (key_h + gap) * 3;
        for (i, ch) in "1234567890".chars().enumerate() {
            let x = m.pad + i as i32 * (unit + gap);
            let label = ch.to_string();
            setup_key(
                host,
                Rect::new(x, y3, unit, key_h),
                &label,
                Style::Plain,
                Hit::SetupChar(ch as u8),
                hits,
            );
        }

        // - _ . centred, the only punctuation GitHub client ids use.
        let y4 = kb_top + (key_h + gap) * 4;
        let row_w = 3 * unit + 2 * gap;
        let row_x = (view.w - row_w) / 2;
        for (i, ch) in [b'-', b'_', b'.'].iter().enumerate() {
            let x = row_x + i as i32 * (unit + gap);
            let label = (*ch as char).to_string();
            setup_key(
                host,
                Rect::new(x, y4, unit, key_h),
                &label,
                Style::Plain,
                Hit::SetupChar(*ch),
                hits,
            );
        }
    }

    // ---- footer ----
    let by = view.h - m.footer_h + (m.footer_h - m.button_h) / 2;
    let bw = (m.pad * 9).min(view.w - 2 * m.pad).max(1);
    let go = Rect::new(m.pad, by, bw, m.button_h);
    host.button(go, "Continue", Style::Primary);
    hits.push((go, Hit::SetupSubmit));

    if v.existing {
        let cancel = Rect::new(m.pad + bw + m.gap, by, m.pad * 6, m.button_h);
        host.button(cancel, "Cancel", Style::Plain);
        hits.push((cancel, Hit::SetupCancel));
    }

    close_button(host, &m, view, hits);
}

pub fn draw_auth<H: Host>(
    host: &mut H,
    v: AuthView<'_>,
    view: View,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let m = Metrics::new(view.w, view.h, view.scale_milli);
    host.fill_rect(Rect::new(0, 0, view.w, view.h), PAPER);

    let left = m.pad * 2;
    let mut y = m.pad * 2;

    let t = baseline(host, y, m.head_px, Face::Title);
    host.text_at(left, t, "grithub", m.head_px, Face::Title, INK);

    // Which OAuth App this device is set up for; the button below changes it.
    if let Some(id) = v.client_id {
        let shown = fit(host, id, view.w / 2, m.body_px, Face::Regular);
        let tw = host.text_width(&shown, m.body_px, Face::Regular);
        let t = baseline(host, m.pad, m.body_px, Face::Regular);
        host.text_at(
            view.w - m.pad - m.button_h - m.gap - tw,
            t,
            &shown,
            m.body_px,
            Face::Regular,
            MUTED,
        );
    }
    y += m.line_h * 2 + m.pad;

    let instruction = match v.phase {
        Phase::SignedOut => "Not signed in.",
        Phase::RequestingCode => "Requesting a sign-in code from GitHub…",
        Phase::AwaitingUser => "On another device, open:",
        Phase::SignedIn => "Signed in.",
        Phase::Denied => "Access was denied.",
        Phase::Expired => "The sign-in code expired.",
        Phase::Failed(_) => "Sign-in did not start.",
    };
    let t = baseline(host, y, m.body_px, Face::Regular);
    host.text_at(left, t, instruction, m.body_px, Face::Regular, MUTED);
    y += m.line_h + m.pad;

    if v.phase == &Phase::AwaitingUser {
        let uri = v.uri.unwrap_or("github.com/login/device");
        let t = baseline(host, y, m.head_px, Face::Bold);
        host.text_at(left, t, uri, m.head_px, Face::Bold, INK);
        y += m.line_h * 2;

        if let Some(code) = v.user_code {
            let box_rect = Rect::new(left, y, (view.w - left * 2).max(1), m.big_px / 1000 * 3);
            host.card(box_rect, Style::Selected);
            let tw = host.text_width(code, m.big_px, Face::FixedBold);
            let t = baseline(host, box_rect.y + m.pad, m.big_px, Face::FixedBold);
            host.text_at(
                box_rect.x + (box_rect.w - tw) / 2,
                t,
                code,
                m.big_px,
                Face::FixedBold,
                INK,
            );
            y = box_rect.bottom() + m.pad;
        }

        let t = baseline(host, y, m.body_px, Face::Regular);
        host.text_at(
            left,
            t,
            "The board appears here as soon as you authorise.",
            m.body_px,
            Face::Regular,
            MUTED,
        );
        y += m.line_h;
    }

    if let Some(status) = v.status {
        let t = baseline(host, y + m.pad, m.body_px, Face::Bold);
        host.text_at(left, t, status, m.body_px, Face::Bold, INK);
    }

    // The probe: whatever the HTTP client actually returned, verbatim.
    if let Some(diag) = v.diagnostics {
        let t = baseline(host, y + m.pad + m.line_h, m.body_px, Face::Regular);
        host.text_at(left, t, diag, m.body_px, Face::Regular, MUTED);
    }

    let by = view.h - m.footer_h + (m.footer_h - m.button_h) / 2;
    let bw = (m.pad * 8).min(view.w - 2 * left).max(1);
    let rect = Rect::new(left, by, bw, m.button_h);
    let label = if v.phase == &Phase::AwaitingUser {
        "Start over"
    } else {
        "Sign in"
    };
    host.button(rect, label, Style::Primary);
    hits.push((rect, Hit::Retry));

    let idw = (m.pad * 7).min(view.w - left - bw - m.pad).max(1);
    let change = Rect::new(left + bw + m.gap, by, idw, m.button_h);
    host.button(change, "Client ID", Style::Plain);
    hits.push((change, Hit::ChangeClient));

    close_button(host, &m, view, hits);
}

pub fn draw_picker<H: Host>(
    host: &mut H,
    v: PickerView<'_>,
    view: View,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let m = Metrics::new(view.w, view.h, view.scale_milli);
    host.fill_rect(Rect::new(0, 0, view.w, view.h), PAPER);

    let t = baseline(host, m.pad / 2, m.head_px, Face::Title);
    host.text_at(m.pad, t, "Projects", m.head_px, Face::Title, INK);

    if let Some(login) = v.login {
        let label = format!("@{login}");
        let tw = host.text_width(&label, m.body_px, Face::Regular);
        let t = baseline(host, m.pad, m.body_px, Face::Regular);
        host.text_at(
            view.w - m.pad - m.button_h - m.gap - tw,
            t,
            &label,
            m.body_px,
            Face::Regular,
            MUTED,
        );
    }

    host.line(0, m.header_h - 2, view.w, m.header_h - 2, 2, RULE);

    let mut y = m.header_h + m.pad;
    if v.projects.is_empty() {
        let msg = if v.syncing {
            "Loading your projects…"
        } else {
            "No projects found for this account."
        };
        let t = baseline(host, y, m.body_px, Face::Regular);
        host.text_at(m.pad, t, msg, m.body_px, Face::Regular, MUTED);
    }

    for project in v.projects {
        let row_h = m.line_h + 2 * m.card_pad;
        if y + row_h > view.h - m.footer_h - m.pad {
            break;
        }
        let rect = Rect::new(m.pad, y, view.w - 2 * m.pad, row_h);
        host.card(rect, Style::Plain);
        let label = format!("#{}  {}", project.number, project.title);
        let t = baseline(host, rect.y + m.card_pad, m.body_px, Face::Regular);
        host.text_at(
            rect.x + m.card_pad,
            t,
            &label,
            m.body_px,
            Face::Regular,
            INK,
        );
        hits.push((rect, Hit::Project(project.id.clone())));
        y += row_h + m.gap;
    }

    if let Some(status) = v.status {
        let t = baseline(host, view.h - m.footer_h - m.line_h, m.body_px, Face::Regular);
        host.text_at(m.pad, t, status, m.body_px, Face::Regular, MUTED);
    }

    let by = view.h - m.footer_h + (m.footer_h - m.button_h) / 2;
    let bw = m.pad * 8;

    let refresh = Rect::new(m.pad, by, bw, m.button_h);
    host.button(refresh, "Refresh", Style::Primary);
    hits.push((refresh, Hit::Refresh));

    let out = Rect::new(view.w - m.pad - bw, by, bw, m.button_h);
    host.button(out, "Sign out", Style::Plain);
    hits.push((out, Hit::SignOut));

    close_button(host, &m, view, hits);
}

pub fn draw_board<H: Host>(
    host: &mut H,
    v: BoardView<'_>,
    view: View,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let m = Metrics::new(view.w, view.h, view.scale_milli);
    let landscape = view.w >= view.h;
    host.fill_rect(Rect::new(0, 0, view.w, view.h), PAPER);

    let per = layout::columns_across(&m, view.w, landscape);
    let pages = layout::page_count(v.board.columns.len(), per);
    let page = v.page.min(pages.saturating_sub(1));

    let title = if v.board.project_title.is_empty() {
        "grithub"
    } else {
        v.board.project_title.as_str()
    };
    let title = fit(
        host,
        title,
        view.w - 2 * m.pad - m.button_h - m.gap,
        m.head_px,
        Face::Title,
    );
    let t = baseline(host, m.pad, m.head_px, Face::Title);
    host.text_at(m.pad, t, &title, m.head_px, Face::Title, INK);
    hits.push((Rect::new(0, 0, view.w / 2, m.header_h), Hit::OpenProjects));

    let right = if let Some(status) = v.status {
        Some((status.to_owned(), INK))
    } else if v.syncing {
        Some(("syncing…".to_owned(), MUTED))
    } else {
        v.login.map(|l| (format!("@{l}"), MUTED))
    };
    if let Some((text, gray)) = right {
        let tw = host.text_width(&text, m.body_px, Face::Regular);
        let t = baseline(host, m.pad, m.body_px, Face::Regular);
        host.text_at(
            view.w - m.pad - m.button_h - m.gap - tw,
            t,
            &text,
            m.body_px,
            Face::Regular,
            gray,
        );
    }

    host.line(0, m.header_h - 2, view.w, m.header_h - 2, 2, RULE);

    let first = page * per;
    for (i, column) in v.board.columns.iter().enumerate().skip(first).take(per) {
        let rect = layout::column_rect(&m, view.w, view.h, i - first, per);
        draw_column(host, &m, rect, column, hits);
    }

    let by = view.h - m.footer_h + (m.footer_h - m.button_h) / 2;
    let bw = m.pad * 5;

    if page > 0 {
        let r = Rect::new(m.pad, by, bw, m.button_h);
        host.button(r, "‹ Prev", Style::Plain);
        hits.push((r, Hit::PrevPage));
    }

    let projects = Rect::new(m.pad + bw + m.gap, by, bw, m.button_h);
    host.button(projects, "Projects", Style::Primary);
    hits.push((projects, Hit::OpenProjects));

    let refresh = Rect::new(m.pad + (bw + m.gap) * 2, by, bw, m.button_h);
    host.button(refresh, "Refresh", Style::Plain);
    hits.push((refresh, Hit::Refresh));

    if pages > 1 {
        let label = format!("{} / {}", page + 1, pages);
        let tw = host.text_width(&label, m.body_px, Face::Regular);
        let t = baseline(host, by, m.body_px, Face::Regular);
        host.text_at(
            view.w - m.pad - tw,
            t,
            &label,
            m.body_px,
            Face::Regular,
            MUTED,
        );
    }

    if page + 1 < pages {
        let r = Rect::new(view.w - m.pad - bw, by - m.button_h - m.gap, bw, m.button_h);
        host.button(r, "Next ›", Style::Plain);
        hits.push((r, Hit::NextPage));
    }

    if v.board.truncated {
        let t = baseline(
            host,
            view.h - m.footer_h - m.line_h,
            m.body_px,
            Face::Regular,
        );
        host.text_at(
            m.pad,
            t,
            "Showing the first 500 cards.",
            m.body_px,
            Face::Regular,
            MUTED,
        );
    }

    close_button(host, &m, view, hits);
}

fn draw_column<H: Host>(
    host: &mut H,
    m: &Metrics,
    rect: Rect,
    column: &Column,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let t = baseline(host, rect.y, m.body_px, Face::Bold);
    host.text_at(rect.x, t, &column.name, m.body_px, Face::Bold, INK);

    let count = column.cards.len().to_string();
    let tw = host.text_width(&count, m.body_px, Face::Regular);
    let t = baseline(host, rect.y, m.body_px, Face::Regular);
    host.text_at(
        rect.right() - tw,
        t,
        &count,
        m.body_px,
        Face::Regular,
        MUTED,
    );

    let rule_y = rect.y + m.col_header_h - m.gap;
    host.line(rect.x, rule_y, rect.right(), rule_y, 2, RULE);

    // Plan first so the measurement borrows end before anything is drawn.
    let title_w = rect.w - 2 * m.card_pad;
    let mut plans: Vec<(Rect, Vec<String>, String)> = Vec::new();
    let mut y = rect.y + m.col_header_h + m.gap;
    let mut hidden = 0usize;

    for card in &column.cards {
        let label = match card.number {
            Some(n) => format!("#{n} {}", card.title),
            None => card.title.clone(),
        };
        let lines = layout::wrap(&label, title_w, 3, &|s: &str| {
            host.text_width(s, m.body_px, Face::Regular)
        });
        let card_h = lines.len() as i32 * m.line_h + 2 * m.card_pad;
        if y + card_h > rect.bottom() - m.line_h {
            hidden += 1;
            continue;
        }
        plans.push((Rect::new(rect.x, y, rect.w, card_h), lines, card.id.clone()));
        y += card_h + m.gap;
    }

    for (r, lines, id) in plans {
        host.card(r, Style::Plain);
        let mut ty = r.y + m.card_pad;
        for line in &lines {
            let t = baseline(host, ty, m.body_px, Face::Regular);
            host.text_at(
                r.x + m.card_pad,
                t,
                line,
                m.body_px,
                Face::Regular,
                INK,
            );
            ty += m.line_h;
        }
        hits.push((r, Hit::Card(id)));
    }

    if hidden > 0 {
        let label = format!("+{hidden} more");
        let t = baseline(host, rect.bottom() - m.line_h, m.body_px, Face::Regular);
        host.text_at(rect.x, t, &label, m.body_px, Face::Regular, MUTED);
    }
}

pub fn draw_detail<H: Host>(
    host: &mut H,
    v: DetailView<'_>,
    view: View,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let m = Metrics::new(view.w, view.h, view.scale_milli);
    host.fill_rect(Rect::new(0, 0, view.w, view.h), PAPER);
    // The whole overlay dismisses, buttons included.
    hits.push((Rect::new(0, 0, view.w, view.h), Hit::CloseDetail));

    let left = m.pad * 2;
    let width = (view.w - left * 2).max(1);
    let mut y = m.pad * 2;

    if !v.project.is_empty() {
        let t = baseline(host, y, m.body_px, Face::Regular);
        host.text_at(left, t, v.project, m.body_px, Face::Regular, MUTED);
        y += m.line_h;
    }

    if let Some(number) = v.card.number {
        let label = format!("#{number}");
        let t = baseline(host, y, m.body_px, Face::Bold);
        host.text_at(left, t, &label, m.body_px, Face::Bold, MUTED);
        y += m.line_h;
    }

    let lines = layout::wrap(&v.card.title, width, 6, &|s: &str| {
        host.text_width(s, m.head_px, Face::Title)
    });
    for line in &lines {
        let t = baseline(host, y, m.head_px, Face::Title);
        host.text_at(left, t, line, m.head_px, Face::Title, INK);
        y += m.line_h;
    }
    y += m.pad;

    if !v.field_name.is_empty() {
        let label = format!("{}: {}", v.field_name, v.column);
        let t = baseline(host, y, m.body_px, Face::Regular);
        host.text_at(left, t, &label, m.body_px, Face::Regular, MUTED);
    }

    let rect = Rect::new(left, view.h - m.footer_h, m.pad * 7, m.button_h);
    host.button(rect, "Close", Style::Primary);
    hits.push((rect, Hit::CloseDetail));

    close_button(host, &m, view, hits);
}
