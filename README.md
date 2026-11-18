# Ferrogram

A lightweight, native Telegram desktop client written in Rust, with a look
modelled on Telegram Desktop's dark theme.

> **Unofficial client.** Ferrogram is not made, endorsed or supported by
> Telegram. It uses the public Telegram API through the pure-Rust
> [grammers](https://codeberg.org/Lonami/grammers) MTProto library. Telegram
> names and trademarks belong to their owners; no Telegram logos, icons or
> fonts are shipped (the UI uses Inter and Lucide icons).

![Chat list and open chat (mock data)](docs/screenshots/chats.png)

## Features

* Login with phone number → code → two-step (cloud) password (SRP).
* Chat list with avatars, last message preview, time, read checks, pinned and
  muted chats, unread badges.
* Infinite-scroll history: the local SQLite cache first, then the network.
* Send and receive text, photos and files in real time (update stream);
  attach with the file chooser or drag & drop.
* Replies, edits, deletes, reactions (right-click a message; double-click to
  reply).
* Search: chats and cached messages in the sidebar, server-side search
  inside a chat.
* Desktop notifications (D-Bus), muted chats and the focused chat excluded.
* Session restored from the OS keyring on start; the auth key is stored only
  as an encrypted blob whose key lives in the keyring.

## Build

Requires Rust stable (edition 2024) and, on Linux, these development
packages:

```sh
sudo apt-get install pkg-config libdbus-1-dev libxkbcommon-dev libwayland-dev \
    libx11-dev libfontconfig1-dev
cargo build --release
```

Windows and macOS builds should work (iced, keyring and notify-rust support
them) but are not the main target.

## Telegram API credentials

Every Telegram client needs an `api_id` and `api_hash`. Ferrogram never ships
any; use your own:

1. Log in at <https://my.telegram.org> with your phone number.
2. Open **API development tools** and create an application (any title).
3. Copy the `api_id` (a number) and `api_hash` (32 hex characters).

Then either export them:

```sh
export FERROGRAM_API_ID=123456
export FERROGRAM_API_HASH=0123456789abcdef0123456789abcdef
```

or put them in `~/.config/ferrogram/config.ini`:

```ini
api_id = 123456
api_hash = 0123456789abcdef0123456789abcdef
```

Never commit these values.

## Run

```sh
./target/release/ferrogram           # real account
./target/release/ferrogram --mock    # demo data, no network, nothing stored
```

`--mock` login codes: `12345` asks for the 2FA password `hunter2`; `22222`
signs in directly. In mock mode "Echo Bot" replies to whatever you send.

Environment variables: `FERRO_THEME=light`, `FERRO_TRACE_STARTUP=1` (logs
`first_frame_ms`), `FERROGRAM_NOTIFY_SENDER_ONLY=1`, `FERROGRAM_NO_NOTIFY=1`,
`RUST_LOG=debug`.

Files: cache `~/.local/share/ferrogram/cache.sqlite3` (safe to delete),
encrypted session `~/.local/share/ferrogram/session.enc`, log
`~/.local/state/ferrogram/log`. A Secret Service provider (KWallet or GNOME
Keyring) is required; without one Ferrogram refuses to store the session.

## Screenshots

Rendered with mock data:

```sh
ICED_BACKEND=tiny-skia xvfb-run -a -s "-screen 0 1280x800x24" \
  ./target/debug/ferrogram --mock --screenshot chats docs/screenshots/chats.png
```

Screens: `login-phone`, `login-code`, `login-password`, `chats`, `search`,
`reply`, `media`.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

See [`docs/architecture.md`](docs/architecture.md) and
[`docs/protocol.md`](docs/protocol.md).

## Licence

MIT. Inter is under the SIL Open Font License 1.1
(`assets/fonts/Inter-LICENSE.txt`); Lucide icons are under the ISC licence
(`assets/icons/LICENSE.txt`).
