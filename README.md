# grithub

A GitHub Projects **V2** kanban board for the Grimoire handwriting notebook: a
full-screen Grimoire *app* (not a widget) that signs in with your own GitHub
OAuth App, fetches a project's items over GraphQL, and draws the board on e-ink.

Status: first implementation slice — setup screen, device-flow sign-in, project
picker, paginated item fetch, board and card-detail rendering. The ABI follows
Grimoire 2.3's `docs/APPS.md`; the logic is unit-tested on the host and the
module builds for `wasm32`, but it has not yet been loaded by a device.

## Install

1. Open **Settings → Packages → Apps → Install from repository...**
2. Enter `nebuff/grithub`.
3. Accept the trust page and install.
4. Open **GitHub Projects** from the home app row or **All apps**.

The app needs the `network:http` and `storage:app-local` capabilities. It hides
the shared status bar so a column gets the whole display.

## First launch: your own OAuth App

No credentials are compiled into the binary. grithub asks GitHub for a device
code, and GitHub only issues one to an OAuth App you own, so the first launch
shows a setup screen instead of a sign-in:

1. On any browser, open <https://github.com/settings/developers> and choose
   **New OAuth App**. The name and homepage are yours; tick **Enable Device
   Flow**.
2. Copy the **Client ID** (no client secret is needed; device flow has none).
3. On the Scribe, type it on grithub's own keyboard — `SHIFT` for capitals,
   `DEL` to correct — and tap **Continue**.

The client ID is public, so it is shown in full and stored in the app's own
`state/runtime.bin`. The token GitHub returns stays on the device too, and the
requested scope is `read:project` (read-only). To point the app at a different
OAuth App later, tap **Client ID** on the sign-in screen.

Sign-in itself is the device flow: grithub shows a `user_code`, you enter it at
`github.com/login/device`, and the board appears as soon as you authorise. Poll
pacing follows GitHub's rules — measured from each response, `slow_down` adds
5 s, and a code expires after 15 minutes.

## Use

- **Projects** lists your Projects V2 boards; tap one to open it.
- Columns follow the project's single-select field (`Status` is preferred), in
  option order, with a trailing **No status** column for items that have none.
- Landscape fits about five columns; portrait pages one column at a time with
  **Next ›** / **‹ Prev**.
- Tap a card for its detail; tap anywhere (or press back) to dismiss.
- **Refresh** re-fetches in the background; a cached board paints immediately
  at launch and refreshes behind you.
- Reference keys map to `app_key` codes 0–3: back, refresh, next, previous.

State lives in `state/runtime.bin`: client id, token, chosen project and the
board cache. `app_stop` flushes it once, so closing the app does not lose the
board or force a new sign-in.

## Build

Rust with the `wasm32-unknown-unknown` target and Python 3 are the only build
requirements.

```sh
rustup target add wasm32-unknown-unknown
make
```

`make` builds `target/wasm32-unknown-unknown/release/grithub.wasm`, copies it to
`app.wasm` at the repository root, and rewrites `artifact.sha256` and
`artifact.size` in `package.yaml` via `tools/package.py`. Publish `app.wasm`
and `package.yaml` together, with a new version, for an update.

```sh
make check   # typecheck host and wasm targets, no artifact written
make test    # host-side unit tests
```

| Path | Role |
|---|---|
| `src/abi.rs` | `wasm32` only. The documented imports/exports. **The only file that changes if the ABI moves.** |
| `src/app.rs` | The state machine: setup, one in-flight request, one step per `app_draw`, `request_redraw` scheduling. |
| `src/auth.rs` | Device flow (RFC 8628) and client-id validation. |
| `src/api.rs` | Request building, status/`errors[]` classification. |
| `src/queries.rs` | GraphQL documents and DTOs. |
| `src/model.rs` | Compact board model, grouping, card cap. |
| `src/store.rs` | Versioned `state/runtime.bin` codec. |
| `src/layout.rs` | Pure geometry and text wrapping. |
| `src/ui.rs` | Screen drawing, on-screen keyboard and hit regions. |
| `src/host.rs` | The `Host` trait plus a mock host for tests. |
| `tools/package.py` | Writes the artifact digest and size into the manifest. |

The app is written against `Host`, not against `extern "C"`, so all behaviour
except the ~250 lines of `abi.rs` is testable with `cargo test` on a laptop.

## ABI (Grimoire 2.3, `docs/APPS.md`)

The parts this app relies on, as documented:

- **Exports:** `app_draw(width, height, scale_milli, elapsed_ms)`,
  `app_pointer(phase, x, y, source)`, `app_key(code, mods, down)`,
  `app_status_bar() -> i32`, `app_stop()`. The module also exports `memory`.
- **One request at a time:** `http_start(method, url, headers, body)` returns
  `0` started, `-1` bad argument, `-2` missing `network:http`, `-3` already
  busy. `http_poll()` returns 0 running / 1 finished; then `http_status`
  (`-1` on failure), `http_body_len`, `http_body`, `http_error`.
- **Timers:** `request_redraw(after_ms)`, 16 ms to 24 h. The host draws only on
  request, so a pending step must schedule the next frame.
- **Drawing:** `fill_rect`/`line` take a gray `0..255`; `text_at(x, baseline,
  …)` takes a baseline and `px_milli`; `card`/`button` take a style (0 plain,
  1 selected, 2 disabled, 3 primary).
- **State:** `state_get`/`state_set` over `state/runtime.bin`, capped at 256 KiB.
  `state/settings.json` belongs to the host.
- **Manifest:** `type: app`, `artifact.path` + `sha256` + `size`,
  `spec.runtime: wasm`, and the two capabilities.

Deliberate choices on top of the ABI:

- `app_status_bar` returns 0 so the board gets the whole display.
- Taps act on `phase == 2` (up), so a press that turns into a scroll does
  nothing.
- The app sends its own `User-Agent`; the ABI's default would otherwise
  identify Grimoire rather than this app.
- `app_key` maps `0..3` to back/refresh/next/previous. The docs do not pin
  gesture codes, so this table is the one thing still worth checking against
  `docs/SHORTCUTS.md`.

## Next steps

- Install a build on a Scribe and watch the first sign-in: on failure the auth
  screen prints the verbatim status and body, which is the cheapest possible
  network probe.
- Organisation-owned projects (`organizations(first:) { projectsV2 }` aliases).
- Card detail: body text and links, once a second query is worth its round trip.
- Refresh on wake, using `request_redraw` rather than polling `wifi_connected`.
