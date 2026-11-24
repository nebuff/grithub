//! The app: one resumable state machine ticked by `app_draw`.
//!
//! Invariants worth keeping when editing this file:
//!
//! * exactly one HTTP request is in flight — the host allows one, and returns
//!   -3 for a second;
//! * `app_draw` advances at most one step and never blocks, because the host
//!   only redraws the app when asked: every pending step ends with
//!   `request_redraw`;
//! * a frame is repainted only when `dirty` is set — on e-ink a redundant full
//!   refresh is the most visible defect the app can have.
//!
//! A first launch has no credentials at all: the app opens on the setup screen,
//! where the user types the public OAuth App client id on the app's own
//! keyboard. Nothing is compiled in, so one build works for any GitHub account.

use std::collections::VecDeque;

use serde_json::json;

use crate::api::{self, ApiError};
use crate::auth::{self, DeviceFlow, Phase, Poll};
use crate::host::{Host, Rect, View};
use crate::layout;
use crate::model::{Board, BoardBuilder, Card, ProjectRef};
use crate::queries;
use crate::store::Persisted;
use crate::ui;

/// `app_pointer` phases: 0 down, 1 move, 2 up. A tap completes on lift.
pub const POINTER_UP: i32 = 2;

/// Key codes are Grimoire's; the mapping to intent is one of the few things the
/// docs leave to the app. See README.md.
pub const KEY_BACK: i32 = 0;
pub const KEY_REFRESH: i32 = 1;
pub const KEY_NEXT: i32 = 2;
pub const KEY_PREV: i32 = 3;

/// While a request runs, ask to be drawn again on this cadence.
const POLL_INTERVAL_MS: u32 = 33;
/// When work is queued but not yet on the wire.
const NEXT_STEP_MS: u32 = 16;
/// Longest client id GitHub issues, and the cap the setup keyboard enforces.
const MAX_CLIENT_ID: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Setup,
    Auth,
    Picker,
    Board,
    Detail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    Retry,
    Refresh,
    SignOut,
    OpenProjects,
    PrevPage,
    NextPage,
    CloseDetail,
    Close,
    ChangeClient,
    SetupChar(u8),
    SetupShift,
    SetupBackspace,
    SetupSubmit,
    SetupCancel,
    Project(String),
    Card(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Task {
    DeviceCode,
    PollToken,
    Projects,
    Fields,
    Items(Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReqKind {
    DeviceCode,
    PollToken,
    Projects,
    Fields,
    Items,
}

pub struct App {
    /// The OAuth App the user entered on the setup screen; empty until then.
    pub client_id: String,
    pub setup_input: String,
    pub setup_shift: bool,
    pub setup_error: Option<String>,
    pub screen: Screen,
    pub flow: DeviceFlow,
    pub token: Option<String>,
    pub login: Option<String>,
    pub projects: Vec<ProjectRef>,
    pub project_id: Option<String>,
    pub project_title: Option<String>,
    pub field_name: Option<String>,
    pub board: Option<Board>,
    builder: Option<BoardBuilder>,
    pub page: usize,
    pub detail: Option<(Card, String)>,
    queue: VecDeque<Task>,
    pub inflight: Option<ReqKind>,
    pub hits: Vec<(Rect, Hit)>,
    pub status: Option<String>,
    pub diagnostics: Option<String>,
    view: View,
    dirty: bool,
}

impl App {
    /// Restore state and decide the first screen. Network work starts from
    /// `frame`, never here: `app_draw` is the only place with a running host.
    pub fn boot<H: Host>(host: &mut H) -> Self {
        let saved = Persisted::decode(&host.state_get());
        let client_id = saved.client_id.clone().unwrap_or_default();

        let mut app = App {
            client_id,
            setup_input: String::new(),
            setup_shift: false,
            setup_error: None,
            screen: Screen::Setup,
            flow: DeviceFlow::new(),
            token: saved.token.clone(),
            login: saved.login.clone(),
            projects: Vec::new(),
            project_id: saved.project_id.clone(),
            project_title: saved.project_title.clone(),
            field_name: saved.field_name.clone(),
            board: None,
            builder: None,
            page: saved.page,
            detail: None,
            queue: VecDeque::new(),
            inflight: None,
            hits: Vec::new(),
            status: None,
            diagnostics: None,
            view: View::default(),
            dirty: true,
        };

        // Nothing can be fetched without knowing which OAuth App to ask.
        if app.client_id.is_empty() {
            return app;
        }

        match (&saved.token, &saved.board) {
            // Stale-while-revalidate: paint the cache now, refresh behind it.
            (Some(_), Some(board)) => {
                app.board = Some(board.clone());
                app.screen = Screen::Board;
                app.queue.push_back(Task::Projects);
            }
            (Some(_), None) => {
                app.screen = Screen::Picker;
                app.queue.push_back(Task::Projects);
            }
            (None, _) => {
                app.screen = Screen::Auth;
                app.queue.push_back(Task::DeviceCode);
            }
        }

        app
    }

    pub fn frame<H: Host>(&mut self, host: &mut H, view: View) {
        if view != self.view {
            self.view = view;
            self.dirty = true;
        }

        self.advance(host);

        if self.dirty {
            self.render(host);
            self.dirty = false;
        }

        self.schedule(host);
    }

    /// Keep the host drawing while this app still has something to do. Without
    /// this the device-flow poll would never fire: the host only draws on
    /// request.
    fn schedule<H: Host>(&mut self, host: &mut H) {
        if self.inflight.is_some() {
            host.request_redraw(POLL_INTERVAL_MS);
        } else if !self.queue.is_empty() {
            host.request_redraw(NEXT_STEP_MS);
        } else if self.flow.is_awaiting() {
            let now = host.now_ms();
            let wait = self.flow.next_poll_at.saturating_sub(now).clamp(50, 5_000);
            host.request_redraw(wait as u32);
        }
    }

    fn advance<H: Host>(&mut self, host: &mut H) {
        let now = host.now_ms();

        if let Some(kind) = self.inflight {
            if !host.http_poll() {
                return;
            }
            self.inflight = None;
            let status = host.http_status();
            let body = host.http_body();
            let error = if status < 0 {
                host.http_error()
            } else {
                String::new()
            };
            self.handle_result(host, kind, status, &body, &error, now);
            return;
        }

        // The setup screen is entirely local: no timers, no queue.
        if self.screen == Screen::Setup {
            return;
        }

        if self.flow.tick(now) {
            self.status = Some("The sign-in code expired. Tap Start over.".to_owned());
            self.dirty = true;
        }

        if self.flow.is_awaiting() && self.queue.is_empty() && self.flow.poll_due(now) {
            self.queue.push_back(Task::PollToken);
        }

        if let Some(task) = self.queue.pop_front() {
            self.start(host, task);
        }
    }

    fn start<H: Host>(&mut self, host: &mut H, task: Task) {
        let client_id = self.client_id.clone();
        let (kind, request) = match task {
            Task::DeviceCode => {
                self.flow.begin();
                (ReqKind::DeviceCode, Some(api::device_code_request(&client_id)))
            }
            Task::PollToken => (
                ReqKind::PollToken,
                self.flow
                    .device_code
                    .as_deref()
                    .map(|dc| api::token_poll_request(&client_id, dc)),
            ),
            Task::Projects => (
                ReqKind::Projects,
                self.token
                    .as_deref()
                    .map(|t| api::graphql_request(t, queries::PROJECTS, json!({}))),
            ),
            Task::Fields => {
                let request = match (&self.token, &self.project_id) {
                    (Some(token), Some(project)) => Some(api::graphql_request(
                        token,
                        queries::FIELDS,
                        json!({ "id": project }),
                    )),
                    _ => None,
                };
                (ReqKind::Fields, request)
            }
            Task::Items(after) => {
                let request = match (&self.token, &self.project_id) {
                    (Some(token), Some(project)) => Some(api::graphql_request(
                        token,
                        queries::ITEMS,
                        json!({ "id": project, "first": 100, "after": after }),
                    )),
                    _ => None,
                };
                (ReqKind::Items, request)
            }
        };

        let Some(request) = request else {
            self.status = Some("Not signed in.".to_owned());
            self.dirty = true;
            return;
        };

        match host.http_start(request) {
            0 => self.inflight = Some(kind),
            -2 => {
                self.status = Some("This app is missing the network:http capability.".to_owned());
            }
            -3 => {
                // The host already has a request; this app only ever starts one.
                self.status = Some("GitHub request already in flight.".to_owned());
            }
            _ => {
                self.status = Some("The request could not start.".to_owned());
            }
        }
        self.dirty = true;
    }

    fn handle_result<H: Host>(
        &mut self,
        host: &mut H,
        kind: ReqKind,
        status: i32,
        body: &[u8],
        error: &str,
        now: u64,
    ) {
        if status < 0 {
            self.on_network_failure(kind, error);
            return;
        }

        let code = status.clamp(0, u16::MAX as i32) as u16;
        match kind {
            ReqKind::DeviceCode => self.on_device_code(code, body, now),
            ReqKind::PollToken => self.on_token(host, code, body, now),
            ReqKind::Projects => self.on_projects(host, code, body, now),
            ReqKind::Fields => self.on_fields(host, code, body, now),
            ReqKind::Items => self.on_items(host, code, body, now),
        }
        self.dirty = true;
    }

    fn on_device_code(&mut self, status: u16, body: &[u8], now: u64) {
        let text = String::from_utf8_lossy(body);

        if status != 200 {
            self.diagnostics = Some(format!(
                "POST /login/device/code → HTTP {status}: {}",
                snippet(&text)
            ));
            self.flow.phase = Phase::Failed(format!("HTTP {status}"));
            self.status = Some("Could not reach GitHub to start sign-in.".to_owned());
            return;
        }

        match self.flow.apply_device_code(&text, now) {
            Ok(()) => {
                self.status = None;
                self.diagnostics = None;
            }
            Err(err) => {
                self.diagnostics = Some(snippet(&text));
                self.status = Some(err);
            }
        }
    }

    fn on_token<H: Host>(&mut self, host: &mut H, status: u16, body: &[u8], now: u64) {
        let text = String::from_utf8_lossy(body);

        if status != 200 {
            self.diagnostics = Some(format!(
                "POST /login/oauth/access_token → HTTP {status}: {}",
                snippet(&text)
            ));
        }

        match self.flow.apply_poll(&text, now) {
            Poll::Authorized(token) => {
                self.token = Some(token);
                self.status = None;
                self.diagnostics = None;
                if self.board.is_none() {
                    self.screen = Screen::Picker;
                }
                self.persist(host);
                self.queue.push_back(Task::Projects);
            }
            Poll::Pending => {}
            Poll::SlowDown => {
                self.status = Some("GitHub asked us to slow down; still waiting.".to_owned());
            }
            Poll::Expired => {
                self.status = Some("The sign-in code expired. Tap Start over.".to_owned());
            }
            Poll::Denied => {
                self.status = Some("Access was denied.".to_owned());
            }
            Poll::Failed(err) => {
                self.status = Some(err);
            }
        }
    }

    fn on_projects<H: Host>(&mut self, host: &mut H, status: u16, body: &[u8], now: u64) {
        let data = match api::classify(status, body) {
            Ok(data) => data,
            Err(err) => return self.handle_api_error(host, err, now),
        };
        let parsed: queries::ProjectsData = match api::decode(&data) {
            Ok(p) => p,
            Err(err) => return self.handle_api_error(host, err, now),
        };

        self.login = Some(parsed.viewer.login);
        self.projects = parsed
            .viewer
            .projects
            .nodes
            .into_iter()
            .map(|node| ProjectRef {
                id: node.id,
                title: node.title,
                number: node.number,
            })
            .collect();

        if self.board.is_none() {
            self.screen = Screen::Picker;
        }

        // Resume the project the user last had open.
        if let Some(project) = self.project_id.clone() {
            if self.projects.iter().any(|p| p.id == project) {
                self.queue.push_back(Task::Fields);
            } else {
                self.project_id = None;
            }
        }

        if self.status.is_some() && self.screen != Screen::Board {
            self.status = None;
        }
        self.persist(host);
    }

    fn on_fields<H: Host>(&mut self, host: &mut H, status: u16, body: &[u8], now: u64) {
        let data = match api::classify(status, body) {
            Ok(data) => data,
            Err(err) => return self.handle_api_error(host, err, now),
        };
        let parsed: queries::FieldsData = match api::decode(&data) {
            Ok(f) => f,
            Err(err) => return self.handle_api_error(host, err, now),
        };

        let node = match parsed.node {
            Some(node) => node,
            None => {
                self.status = Some("That project is not visible to this token.".to_owned());
                return;
            }
        };

        let (field_name, options) = match queries::pick_board_field(&node.fields.nodes) {
            Some(found) => found,
            None => {
                self.status =
                    Some("This project has no single-select field to build columns from.".to_owned());
                return;
            }
        };

        if let Some(title) = node.title {
            self.project_title = Some(title);
        }
        self.field_name = Some(field_name.clone());
        self.builder = Some(BoardBuilder::new(
            self.project_id.as_deref().unwrap_or_default(),
            self.project_title.as_deref().unwrap_or_default(),
            &field_name,
            &options,
        ));
        self.page = 0;
        self.queue.push_back(Task::Items(None));
        self.persist(host);
    }

    fn on_items<H: Host>(&mut self, host: &mut H, status: u16, body: &[u8], now: u64) {
        let data = match api::classify(status, body) {
            Ok(data) => data,
            Err(err) => return self.handle_api_error(host, err, now),
        };
        let parsed: queries::ItemsData = match api::decode(&data) {
            Ok(i) => i,
            Err(err) => return self.handle_api_error(host, err, now),
        };

        let step = match self.builder.as_mut() {
            Some(builder) => builder.push_page(&parsed),
            None => return,
        };

        if step.has_next {
            if let Some(cursor) = step.cursor {
                self.queue.push_back(Task::Items(Some(cursor)));
            }
        } else if let Some(builder) = self.builder.take() {
            self.board = Some(builder.finish(now));
            self.screen = Screen::Board;
            self.status = None;
            self.persist(host);
        }
    }

    fn handle_api_error<H: Host>(&mut self, host: &mut H, err: ApiError, now: u64) {
        match err {
            ApiError::Unauthorized => {
                self.sign_out(host);
                self.flow.begin();
                self.screen = Screen::Auth;
                self.queue.push_back(Task::DeviceCode);
                self.status = Some("GitHub rejected the saved token. Sign in again.".to_owned());
            }
            ApiError::RateLimited => {
                self.status = Some("GitHub rate limit reached; try again shortly.".to_owned());
                self.diagnostics = Some(format!("rate limited at t={now} ms"));
            }
            ApiError::NotFound => {
                self.status = Some("Project not found or not visible to this token.".to_owned());
                self.project_id = None;
                self.screen = Screen::Picker;
            }
            ApiError::GraphQl(message) => self.status = Some(message),
            ApiError::Http(code) => self.status = Some(format!("GitHub returned HTTP {code}.")),
            ApiError::Malformed(detail) => {
                self.status = Some("Unexpected response from GitHub.".to_owned());
                self.diagnostics = Some(detail);
            }
        }
        self.dirty = true;
    }

    fn on_network_failure(&mut self, kind: ReqKind, error: &str) {
        if kind == ReqKind::DeviceCode {
            self.flow.phase = Phase::Failed("network error".to_owned());
        }
        self.status = Some("Can't reach GitHub. Check the connection and retry.".to_owned());
        self.diagnostics = if error.is_empty() {
            None
        } else {
            Some(error.to_owned())
        };
        self.dirty = true;
    }

    fn sign_out<H: Host>(&mut self, host: &mut H) {
        self.token = None;
        self.login = None;
        self.project_id = None;
        self.project_title = None;
        self.field_name = None;
        self.board = None;
        self.builder = None;
        self.detail = None;
        self.page = 0;
        self.persist(host);
    }

    fn persist<H: Host>(&self, host: &mut H) {
        let saved = Persisted {
            client_id: (!self.client_id.is_empty()).then(|| self.client_id.clone()),
            token: self.token.clone(),
            login: self.login.clone(),
            project_id: self.project_id.clone(),
            project_title: self.project_title.clone(),
            field_name: self.field_name.clone(),
            board: self.board.clone(),
            page: self.page,
            ..Persisted::default()
        };
        host.state_set(&saved.encode());
    }

    // ---- the setup keyboard ----

    fn setup_push(&mut self, byte: u8) {
        if self.setup_input.len() >= MAX_CLIENT_ID {
            return;
        }
        let ch = byte as char;
        let ch = if self.setup_shift {
            ch.to_ascii_uppercase()
        } else {
            ch
        };
        self.setup_input.push(ch);
        self.setup_error = None;
        self.dirty = true;
    }

    fn setup_backspace(&mut self) {
        if self.setup_input.pop().is_some() {
            self.setup_error = None;
            self.dirty = true;
        }
    }

    /// Store the client id and start sign-in. A changed id means the old token
    /// was issued by a different OAuth App, so it is dropped rather than reused.
    fn finish_setup<H: Host>(&mut self, host: &mut H) {
        let id = match auth::normalize_client_id(&self.setup_input) {
            Ok(id) => id,
            Err(message) => {
                self.setup_error = Some(message.to_owned());
                self.dirty = true;
                return;
            }
        };

        let changed = id != self.client_id;
        self.client_id = id;
        self.setup_input.clear();
        self.setup_error = None;
        self.setup_shift = false;

        if changed {
            self.sign_out(host);
        }
        self.persist(host);

        if self.token.is_none() {
            self.flow.reset();
            self.flow.begin();
            self.screen = Screen::Auth;
            self.queue.push_back(Task::DeviceCode);
        } else {
            self.screen = if self.board.is_some() {
                Screen::Board
            } else {
                Screen::Picker
            };
            self.queue.push_back(Task::Projects);
        }
        self.dirty = true;
    }

    /// `phase` is 0 down, 1 move, 2 up. Acting on lift is what the ABI's
    /// phase model implies for a tap.
    pub fn on_pointer<H: Host>(&mut self, host: &mut H, x: i32, y: i32, phase: i32) {
        if phase != POINTER_UP {
            return;
        }

        let hit = self
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(x, y))
            .map(|(_, hit)| hit.clone());

        if let Some(hit) = hit {
            self.dispatch(host, hit);
        }
    }

    pub fn on_key<H: Host>(&mut self, _host: &mut H, key: i32) {
        match key {
            KEY_BACK => self.back(),
            KEY_REFRESH => {
                if self.screen == Screen::Setup {
                    return;
                }
                self.status = None;
                self.queue.push_back(Task::Projects);
            }
            KEY_NEXT => {
                if self.screen != Screen::Board {
                    return;
                }
                self.page += 1;
            }
            KEY_PREV => self.page = self.page.saturating_sub(1),
            _ => return,
        }
        self.dirty = true;
    }

    fn dispatch<H: Host>(&mut self, host: &mut H, hit: Hit) {
        match hit {
            Hit::Close => host.close(),
            Hit::Retry => {
                self.diagnostics = None;
                self.status = None;
                if self.token.is_some() {
                    self.queue.push_back(Task::Projects);
                } else {
                    self.flow.reset();
                    self.flow.begin();
                    self.screen = Screen::Auth;
                    self.queue.push_back(Task::DeviceCode);
                }
            }
            Hit::Refresh => {
                self.status = None;
                self.queue.push_back(Task::Projects);
            }
            Hit::SignOut => {
                self.sign_out(host);
                self.flow.reset();
                self.flow.begin();
                self.screen = Screen::Auth;
                self.queue.push_back(Task::DeviceCode);
            }
            Hit::OpenProjects => {
                self.screen = Screen::Picker;
                self.queue.push_back(Task::Projects);
            }
            Hit::PrevPage => self.page = self.page.saturating_sub(1),
            Hit::NextPage => self.page += 1,
            Hit::CloseDetail => {
                self.detail = None;
                self.screen = Screen::Board;
            }
            Hit::ChangeClient => {
                // Any in-flight sign-in was for the old app id.
                self.flow.reset();
                self.queue.clear();
                self.setup_input = self.client_id.clone();
                self.setup_shift = false;
                self.setup_error = None;
                self.screen = Screen::Setup;
            }
            Hit::SetupChar(byte) => self.setup_push(byte),
            Hit::SetupShift => self.setup_shift = !self.setup_shift,
            Hit::SetupBackspace => self.setup_backspace(),
            Hit::SetupSubmit => self.finish_setup(host),
            Hit::SetupCancel => {
                if !self.client_id.is_empty() {
                    self.screen = Screen::Auth;
                }
            }
            Hit::Project(id) => {
                if let Some(project) = self.projects.iter().find(|p| p.id == id) {
                    self.project_id = Some(project.id.clone());
                    self.project_title = Some(project.title.clone());
                }
                self.board = None;
                self.builder = None;
                self.queue.push_back(Task::Fields);
            }
            Hit::Card(id) => {
                if let Some(board) = self.board.as_ref() {
                    if let Some((card, column)) = board.find_card(&id) {
                        let card = card.clone();
                        let column = column.name.clone();
                        self.detail = Some((card, column));
                        self.screen = Screen::Detail;
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn back(&mut self) {
        match self.screen {
            Screen::Detail => {
                self.detail = None;
                self.screen = Screen::Board;
            }
            Screen::Board => self.screen = Screen::Picker,
            Screen::Picker | Screen::Auth => {}
            Screen::Setup => {
                if !self.client_id.is_empty() {
                    self.screen = Screen::Auth;
                } else {
                    return;
                }
            }
        }
        self.dirty = true;
    }

    fn render<H: Host>(&mut self, host: &mut H) {
        self.hits.clear();
        let view = self.view;

        match self.screen {
            Screen::Setup => {
                let v = ui::SetupView {
                    input: &self.setup_input,
                    shift: self.setup_shift,
                    error: self.setup_error.as_deref(),
                    existing: !self.client_id.is_empty(),
                };
                ui::draw_setup(host, v, view, &mut self.hits);
            }
            Screen::Auth => {
                let v = ui::AuthView {
                    phase: &self.flow.phase,
                    user_code: self.flow.user_code.as_deref(),
                    uri: self.flow.verification_uri.as_deref(),
                    status: self.status.as_deref(),
                    diagnostics: self.diagnostics.as_deref(),
                    client_id: (!self.client_id.is_empty()).then_some(self.client_id.as_str()),
                };
                ui::draw_auth(host, v, view, &mut self.hits);
            }
            Screen::Picker => {
                let v = ui::PickerView {
                    projects: &self.projects,
                    login: self.login.as_deref(),
                    status: self.status.as_deref(),
                    syncing: self.inflight.is_some(),
                };
                ui::draw_picker(host, v, view, &mut self.hits);
            }
            Screen::Board => {
                let metrics = layout::Metrics::new(view.w, view.h, view.scale_milli);
                let per = layout::columns_across(&metrics, view.w, view.w >= view.h);
                let columns = self.board.as_ref().map(|b| b.columns.len()).unwrap_or(0);
                let pages = layout::page_count(columns, per);
                if self.page >= pages {
                    self.page = pages - 1;
                }

                if let Some(board) = self.board.as_ref() {
                    let v = ui::BoardView {
                        board,
                        page: self.page,
                        login: self.login.as_deref(),
                        status: self.status.as_deref(),
                        syncing: self.inflight.is_some(),
                    };
                    ui::draw_board(host, v, view, &mut self.hits);
                }
            }
            Screen::Detail => {
                if let Some((card, column)) = self.detail.as_ref() {
                    let v = ui::DetailView {
                        card,
                        column: column.as_str(),
                        project: self.project_title.as_deref().unwrap_or(""),
                        field_name: self.field_name.as_deref().unwrap_or(""),
                    };
                    ui::draw_detail(host, v, view, &mut self.hits);
                } else {
                    self.screen = Screen::Board;
                    self.dirty = true;
                }
            }
        }
    }

    /// Called exactly once before the app exits: the last chance to persist.
    pub fn on_stop<H: Host>(&mut self, host: &mut H) {
        self.persist(host);
    }
}

fn snippet(body: &str) -> String {
    let cleaned: String = body.chars().take(160).collect();
    cleaned.replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::mock::MockHost;
    use crate::store::SCHEMA_VERSION;

    const VIEW: View = View {
        w: 2480,
        h: 1860,
        scale_milli: 1000,
    };

    const CLIENT: &str = "Iv1.0123456789abcdef";

    const DEVICE: &str = r#"{"device_code":"dc","user_code":"BFEE-895F","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#;
    const TOKEN: &str =
        r#"{"access_token":"gho_x","token_type":"bearer","scope":"read:project"}"#;

    /// GitHub wraps every GraphQL payload in `{"data": …}`; the fixtures keep
    /// that shape so the classifier is exercised on the real wire format.
    fn gql(data: &str) -> String {
        format!(r#"{{"data":{data}}}"#)
    }

    const PROJECTS_DATA: &str = r#"{"viewer":{"login":"octocat","projectsV2":{"nodes":[
        {"id":"P_1","title":"Roadmap","number":1},
        {"id":"P_2","title":"Bugs","number":2}
    ]}}}"#;

    const FIELDS_DATA: &str = r#"{"node":{"title":"Roadmap","fields":{"nodes":[
        {"__typename":"ProjectV2Field","name":"Title"},
        {"__typename":"ProjectV2SingleSelectField","name":"Status",
         "options":[{"name":"Todo"},{"name":"In Progress"},{"name":"Done"}]}
    ]}}}"#;

    const ITEMS_DATA: &str = r#"{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
        {"id":"i1","content":{"__typename":"Issue","number":12,"title":"Fix the flux capacitor"},
         "fieldValues":{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"Done","field":{"name":"Status"}}]}},
        {"id":"i2","content":{"__typename":"DraftIssue","title":"Write the changelog"},"fieldValues":{"nodes":[]}},
        {"id":"i3","content":{"__typename":"Redacted"},"fieldValues":null}
    ]}}}"#;

    /// A device that has already been through the setup screen.
    fn boot_with_client(host: &mut MockHost) -> App {
        host.state = Persisted {
            client_id: Some(CLIENT.to_owned()),
            ..Persisted::default()
        }
        .encode();
        App::boot(host)
    }

    fn tap(app: &mut App, host: &mut MockHost, want: Hit) {
        let rect = app
            .hits
            .iter()
            .rev()
            .find(|(_, hit)| *hit == want)
            .map(|(rect, _)| *rect)
            .unwrap_or_else(|| panic!("no hit region for {want:?}"));
        app.on_pointer(host, rect.x + 2, rect.y + 2, POINTER_UP);
        app.frame(host, VIEW);
    }

    /// Type on the app's own keyboard, holding shift where the text needs it.
    fn type_on_setup(app: &mut App, host: &mut MockHost, text: &str) {
        for ch in text.chars() {
            if ch.is_ascii_uppercase() != app.setup_shift {
                tap(app, host, Hit::SetupShift);
            }
            tap(app, host, Hit::SetupChar(ch.to_ascii_lowercase() as u8));
        }
    }

    /// Drive the whole sign-in -> board flow. Landscape, because portrait
    /// deliberately pages one column at a time.
    fn signed_in_app() -> (App, MockHost) {
        let mut host = MockHost::new();
        host.push(200, DEVICE);
        let mut app = boot_with_client(&mut host);

        app.frame(&mut host, VIEW); // start device-code request
        assert_eq!(app.inflight, Some(ReqKind::DeviceCode));
        app.frame(&mut host, VIEW); // complete it

        host.now += 5_000;
        host.push(200, TOKEN);
        app.frame(&mut host, VIEW); // interval elapsed: poll
        app.frame(&mut host, VIEW); // authorised

        let projects = gql(PROJECTS_DATA);
        host.push(200, &projects);
        app.frame(&mut host, VIEW);
        app.frame(&mut host, VIEW);

        (app, host)
    }

    fn open_first_project(app: &mut App, host: &mut MockHost) {
        let row = app
            .hits
            .iter()
            .find_map(|(rect, hit)| matches!(hit, Hit::Project(_)).then_some(*rect))
            .expect("project row should be tappable");
        app.on_pointer(host, row.x + 4, row.y + 4, POINTER_UP);

        let fields = gql(FIELDS_DATA);
        host.push(200, &fields);
        app.frame(host, VIEW);
        app.frame(host, VIEW);

        let items = gql(ITEMS_DATA);
        host.push(200, &items);
        app.frame(host, VIEW);
        app.frame(host, VIEW);
    }

    #[test]
    fn a_fresh_install_opens_setup_and_stays_off_the_network() {
        let mut host = MockHost::new();
        let mut app = App::boot(&mut host);
        app.frame(&mut host, VIEW);

        assert_eq!(app.screen, Screen::Setup);
        assert!(host.requests.is_empty(), "setup must not call GitHub");
        assert!(
            host.redraws.is_empty(),
            "nothing is pending, so nothing should be scheduled"
        );
        assert!(app.hits.iter().any(|(_, hit)| *hit == Hit::SetupSubmit));
        assert!(app.hits.iter().any(|(_, hit)| *hit == Hit::SetupChar(b'i')));
    }

    #[test]
    fn the_setup_screen_types_a_client_id_and_starts_sign_in() {
        let mut host = MockHost::new();
        let mut app = App::boot(&mut host);
        app.frame(&mut host, VIEW);

        type_on_setup(&mut app, &mut host, "Iv1.01234");
        assert_eq!(app.setup_input, "Iv1.01234");
        assert!(host.requests.is_empty(), "typing must not hit the network");

        host.push(200, DEVICE);
        tap(&mut app, &mut host, Hit::SetupSubmit);
        assert_eq!(app.screen, Screen::Auth);
        assert_eq!(app.inflight, Some(ReqKind::DeviceCode));
        assert_eq!(
            Persisted::decode(&host.state).client_id.as_deref(),
            Some("Iv1.01234")
        );

        app.frame(&mut host, VIEW);
        assert_eq!(app.flow.user_code.as_deref(), Some("BFEE-895F"));
    }

    #[test]
    fn the_close_button_leaves_the_app() {
        let mut host = MockHost::new();
        let mut app = App::boot(&mut host);
        app.frame(&mut host, VIEW);

        tap(&mut app, &mut host, Hit::Close);

        assert!(host.closed, "✕ must ask the host to close the app");
    }

    #[test]
    fn the_client_id_survives_a_relaunch_without_being_compiled_in() {
        let mut host = MockHost::new();
        let mut app = App::boot(&mut host);
        app.frame(&mut host, VIEW);
        type_on_setup(&mut app, &mut host, "Ov23liAbCdEf");
        host.push(200, DEVICE);
        tap(&mut app, &mut host, Hit::SetupSubmit);

        let relaunched = App::boot(&mut host);
        assert_eq!(relaunched.client_id, "Ov23liAbCdEf");
        assert_eq!(relaunched.screen, Screen::Auth);
    }

    #[test]
    fn a_mistyped_client_id_is_refused_before_any_request() {
        let mut host = MockHost::new();
        let mut app = App::boot(&mut host);
        app.frame(&mut host, VIEW);

        type_on_setup(&mut app, &mut host, "abc");
        tap(&mut app, &mut host, Hit::SetupSubmit);

        assert_eq!(app.screen, Screen::Setup);
        assert!(app.setup_error.is_some());
        assert!(host.requests.is_empty());
    }

    #[test]
    fn the_shift_key_uppercases_and_backspace_undoes_typing() {
        let mut host = MockHost::new();
        let mut app = App::boot(&mut host);
        app.frame(&mut host, VIEW);

        tap(&mut app, &mut host, Hit::SetupShift);
        tap(&mut app, &mut host, Hit::SetupChar(b'i'));
        assert_eq!(app.setup_input, "I");

        tap(&mut app, &mut host, Hit::SetupBackspace);
        assert_eq!(app.setup_input, "");
    }

    #[test]
    fn device_flow_shows_the_code_and_stores_the_token() {
        let (app, host) = signed_in_app();
        assert_eq!(app.token.as_deref(), Some("gho_x"));
        assert_eq!(app.screen, Screen::Picker);
        assert_eq!(app.login.as_deref(), Some("octocat"));
        assert_eq!(app.projects.len(), 2);
        assert_eq!(Persisted::decode(&host.state).token.as_deref(), Some("gho_x"));
    }

    #[test]
    fn the_user_code_is_actually_drawn() {
        let mut host = MockHost::new();
        host.push(200, DEVICE);
        let mut app = boot_with_client(&mut host);
        app.frame(&mut host, VIEW);
        app.frame(&mut host, VIEW);
        assert!(host.drew("BFEE-895F"));
        assert!(host.drew("github.com/login/device"));
    }

    #[test]
    fn every_request_is_a_post_with_its_method_on_the_wire() {
        let (_, host) = signed_in_app();
        assert!(!host.requests.is_empty());
        for request in &host.requests {
            assert_eq!(request.method, "POST", "{:?}", request.url);
            assert!(!request.url.is_empty());
        }
    }

    #[test]
    fn a_running_request_asks_for_another_frame() {
        let mut host = MockHost::new();
        host.push(200, DEVICE);
        host.stall_next(3);
        let mut app = boot_with_client(&mut host);

        app.frame(&mut host, VIEW); // starts the request
        for _ in 0..3 {
            app.frame(&mut host, VIEW);
            assert_eq!(host.last_redraw(), Some(POLL_INTERVAL_MS));
        }
        app.frame(&mut host, VIEW); // finishes
        assert!(app.inflight.is_none());
    }

    #[test]
    fn an_idle_board_does_not_ask_for_frames() {
        let (mut app, mut host) = signed_in_app();
        open_first_project(&mut app, &mut host);
        assert_eq!(app.screen, Screen::Board);

        let before = host.redraws.len();
        app.frame(&mut host, VIEW);
        assert_eq!(host.redraws.len(), before, "an up-to-date board must idle");
    }

    #[test]
    fn selecting_a_project_builds_columns_in_field_order() {
        let (mut app, mut host) = signed_in_app();
        open_first_project(&mut app, &mut host);

        assert_eq!(app.screen, Screen::Board);
        let board = app.board.as_ref().expect("board");
        assert_eq!(
            board.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Todo", "In Progress", "Done", "No status"]
        );
        assert_eq!(board.columns[2].cards[0].title, "Fix the flux capacitor");
        assert_eq!(board.columns[3].cards.len(), 2);
        assert_eq!(board.fetched_at, host.now);
    }

    #[test]
    fn tapping_a_card_opens_the_detail_overlay_and_back_closes_it() {
        let (mut app, mut host) = signed_in_app();
        open_first_project(&mut app, &mut host);

        let card = app
            .hits
            .iter()
            .find_map(|(rect, hit)| matches!(hit, Hit::Card(_)).then_some(*rect))
            .expect("card should be tappable");
        app.on_pointer(&mut host, card.x + 3, card.y + 3, POINTER_UP);
        assert_eq!(app.screen, Screen::Detail);

        app.frame(&mut host, VIEW);
        assert!(host.drew("Close"));

        app.on_key(&mut host, KEY_BACK);
        assert_eq!(app.screen, Screen::Board);
    }

    #[test]
    fn a_pointer_move_is_not_a_tap() {
        let (mut app, mut host) = signed_in_app();
        open_first_project(&mut app, &mut host);

        let card = app
            .hits
            .iter()
            .find_map(|(rect, hit)| matches!(hit, Hit::Card(_)).then_some(*rect))
            .unwrap();
        app.on_pointer(&mut host, card.x + 3, card.y + 3, 1);
        assert_eq!(app.screen, Screen::Board);
    }

    #[test]
    fn a_cached_board_paints_before_any_network_round_trip() {
        let mut host = MockHost::new();
        let mut builder = BoardBuilder::new("P_1", "Roadmap", "Status", &["Todo".to_owned()]);
        builder.push_page(&serde_json::from_str::<queries::ItemsData>(ITEMS_DATA).unwrap());
        host.state = Persisted {
            client_id: Some(CLIENT.to_owned()),
            token: Some("gho_x".to_owned()),
            login: Some("octocat".to_owned()),
            project_id: Some("P_1".to_owned()),
            project_title: Some("Roadmap".to_owned()),
            field_name: Some("Status".to_owned()),
            board: Some(builder.finish(7)),
            ..Persisted::default()
        }
        .encode();

        let mut app = App::boot(&mut host);
        assert_eq!(app.screen, Screen::Board);

        app.frame(&mut host, VIEW);
        assert!(host.drew("card:"), "cached cards should paint immediately");
    }

    #[test]
    fn a_rejected_token_signs_out_and_restarts_the_device_flow() {
        let mut host = MockHost::new();
        host.push(200, DEVICE);
        let mut app = boot_with_client(&mut host);
        app.frame(&mut host, VIEW);
        app.frame(&mut host, VIEW);
        host.now += 5_000;
        host.push(200, TOKEN);
        app.frame(&mut host, VIEW);
        app.frame(&mut host, VIEW);

        host.push(401, "{}");
        host.push(200, DEVICE);
        app.frame(&mut host, VIEW); // starts Projects
        app.frame(&mut host, VIEW); // 401 -> sign out + restart

        assert!(app.token.is_none());
        assert_eq!(app.screen, Screen::Auth);

        app.frame(&mut host, VIEW); // fresh device-code request
        app.frame(&mut host, VIEW);
        assert_eq!(app.flow.user_code.as_deref(), Some("BFEE-895F"));
    }

    #[test]
    fn a_dead_connection_lands_on_an_actionable_screen() {
        let mut host = MockHost::new();
        host.push_failure("Could not resolve host: github.com");
        let mut app = boot_with_client(&mut host);
        app.frame(&mut host, VIEW);
        app.frame(&mut host, VIEW);

        assert!(matches!(app.flow.phase, Phase::Failed(_)));
        assert!(app.status.as_deref().unwrap_or("").contains("Can't reach GitHub"));
        assert!(app
            .diagnostics
            .as_deref()
            .unwrap_or("")
            .contains("Could not resolve host"));
        assert!(app.hits.iter().any(|(_, hit)| *hit == Hit::Retry));
    }

    #[test]
    fn a_missing_capability_is_reported_rather_than_retried_forever() {
        let mut host = MockHost::new();
        host.push_start_error(-2);
        let mut app = boot_with_client(&mut host);
        app.frame(&mut host, VIEW);

        assert!(app.inflight.is_none());
        assert!(app
            .status
            .as_deref()
            .unwrap_or("")
            .contains("network:http"));
    }

    #[test]
    fn state_round_trips_across_a_relaunch() {
        let (app, host) = signed_in_app();
        let saved = Persisted::decode(&host.state);
        assert_eq!(saved.v, SCHEMA_VERSION);
        assert_eq!(saved.project_id, app.project_id);
        assert_eq!(saved.login.as_deref(), Some("octocat"));
    }
}
