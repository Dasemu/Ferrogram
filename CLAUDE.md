# Ferrogram

## Overview
Native Rust Telegram desktop client (iced 0.14 UI, grammers-client 0.12
MTProto, rusqlite cache, keyring secrets). Looks like Telegram Desktop "Night".
Unofficial; never ship Telegram assets.

## Commands
- `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `dbus-run-session -- cargo test` (the tray test needs a bus; it skips without one)
- `cargo run -- --mock` runs on fake data (code 12345 + password hunter2).
- Screenshot: `ICED_BACKEND=tiny-skia xvfb-run -a -s "-screen 0 1280x800x24" ./target/debug/ferrogram --mock --screenshot <screen> out.png`
- Shared machine: `export CARGO_BUILD_JOBS=2`.
- `ferrogram --version` (built-in creds?), `--api-setup`, `--install-desktop-entry`.

## Architecture
See `docs/architecture.md` (shared Ferro* skeleton + §11 app specifics) and
`docs/protocol.md` (grammers, TL conversion, session vault).
`ui → app → {net, store, auth}`; `net::TelegramApi` is the only door to Telegram.

## Conventions
- English everywhere; Conventional Commits; small commits.
- No `unwrap`/`expect` outside tests (clippy denies them).
- Views never hard-code colours/sizes: use `ui/theme.rs`.
- Never log phone numbers, bodies, names, keys: use `Phone`, `Redacted`,
  manual `Debug` impls. `logging::scrub` is only a safety net.
- Only `auth/secrets.rs` may touch the `keyring` crate.
- New crates need a one-line justification in the PR.

## Workflow quirks
- Work on feature branches, PR into `development`; never push `main`/`development`.
- `version.yml` (helper `.github/scripts/version.sh`) bumps `Cargo.toml`; don't bump by hand.
  After a bump on `main` it MERGES main into `development` (real merge, no sync commit).
- Raw TL structs change per layer: keep them confined to `net/types.rs` and tests.

## Background mode
Tray (`ksni`, StatusNotifierItem) + `iced::daemon`: with a tray host, the close
button closes the window (frees the renderer) and the app keeps running. Pure
state machine in `app/background.rs`; tray in `src/tray/`; second launch via
`instance.rs`; `--minimized`; autostart in `autostart.rs`. See architecture 11.4.

## Deployment
Releases are automatic: `.github/workflows/release.yml` chains off the "Version" workflow
via `workflow_run` (GITHUB_TOKEN bot pushes don't trigger `push` workflows), builds on
ubuntu-22.04, and creates GitHub Release `v<Cargo version>` with `ferrogram-v<ver>-linux-x86_64.tar.gz`
+ `.sha256` (skips if the release exists; manual re-run: workflow_dispatch). Users run
`install.sh` (`curl ... | sh`; `-s -- --uninstall`); test it with `FERRO_RELEASE_API=file://...`.
Builds embed `TELEGRAM_API_ID`/`TELEGRAM_API_HASH` repo secrets via `build.rs` (fails if unset);
a `v*` tag push also releases.

## Gotchas
- Tray/instance/notification clicks reach the app only through `remote::send`;
  never call `App` from those threads. No tray host => closing must quit.
- Keep iced default features (x11/wayland backends); `default-features=false` breaks softbuffer.
- `grammers-session` default features pull `libsql` (second SQLite, plain-text session): keep them off.
- `history` scrollable is `anchor_bottom`: offset 0 = newest; "top" math is in `state::distance_from_top`.
- Emoji need the `Noto Color Emoji` font span (`widgets::emoji_text`), else monochrome fallbacks.
- Containers holding a `Fill` row still need `.width(Fill)` or their background shrinks.
- `Dialog::peer_ref()` in grammers unwraps; use `peer().to_ref().await` instead.
- Credentials: override (keyring `api-hash` + config.ini `api_id`) → env → embedded. Never commit real values.
- Logout/revoke must rebuild `GrammersApi` (fresh SenderPool + EventHub subscription).
