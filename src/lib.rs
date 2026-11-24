//! grithub — a GitHub Projects V2 kanban board viewer for the Grimoire
//! handwriting notebook.
//!
//! Layer map:
//!
//! * `abi` (wasm only) — imports/exports, the sole contact with the host;
//! * `app` — the resumable state machine ticked by `app_draw`;
//! * `auth` — GitHub OAuth device flow;
//! * `api` / `queries` — GraphQL transport and response shapes;
//! * `model` / `store` — compact board model and versioned persistence;
//! * `layout` / `ui` — geometry and drawing, host-agnostic via `Host`.

pub mod api;
pub mod app;
pub mod auth;
pub mod host;
pub mod layout;
pub mod model;
pub mod queries;
pub mod store;
pub mod ui;

#[cfg(target_arch = "wasm32")]
mod abi;

pub use app::{App, Hit, Screen};
