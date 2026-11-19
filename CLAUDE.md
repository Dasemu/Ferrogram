# Ferrogram

## Overview
Native Rust Telegram desktop client (iced 0.14 UI, grammers-client 0.12
MTProto, rusqlite cache, keyring secrets). Looks like Telegram Desktop "Night".
Unofficial; never ship Telegram assets.

## Commands
- `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `cargo test`
- `cargo run -- --mock` runs on fake data (code 12345 + password hunter2).
- Screenshot: `ICED_BACKEND=tiny-skia xvfb-run -a -s "-screen 0 1280x800x24" ./target/debug/ferrogram --mock --screenshot <screen> out.png`
- Shared machine: `export CARGO_BUILD_JOBS=2`.

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
- `version.yml` bumps `Cargo.toml` on pushes to `development`/`main`; don't bump by hand.
- Raw TL structs change per layer: keep them confined to `net/types.rs` and tests.

## Deployment
No packaging yet: `cargo build --release` gives `target/release/ferrogram`.
Users supply `FERROGRAM_API_ID`/`FERROGRAM_API_HASH` (my.telegram.org).

## Gotchas
- Keep iced default features (x11/wayland backends); `default-features=false` breaks softbuffer.
- `grammers-session` default features pull `libsql` (second SQLite, plain-text session): keep them off.
- `history` scrollable is `anchor_bottom`: offset 0 = newest; "top" math is in `state::distance_from_top`.
- Emoji need the `Noto Color Emoji` font span (`widgets::emoji_text`), else monochrome fallbacks.
- Containers holding a `Fill` row still need `.width(Fill)` or their background shrinks.
- `Dialog::peer_ref()` in grammers unwraps; use `peer().to_ref().await` instead.
- api_hash lives only in the keyring (`api-hash`); never add it back to config.ini.
- Logout/revoke must rebuild `GrammersApi` (fresh SenderPool + EventHub subscription).
