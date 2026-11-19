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
  reply). Edits and deletes are optimistic and roll back if the server
  refuses.
* Received files and photos can be saved with **Save As…** in the context
  menu, or by clicking a file's icon.
* Attachments (from the file chooser or drag & drop) show a preview bar and
  are only sent after you confirm.
* Search: chats and cached messages in the sidebar, server-side search
  inside a chat.
* Desktop notifications (D-Bus), muted chats and the focused chat excluded.
* The session is restored from the OS keyring on start. The auth key is stored
  only as an encrypted blob whose key lives in the keyring.
* Offline start: with a saved session and no network, the cached chats stay
  visible and the session restore is retried with backoff. You are never
  sent back to the phone screen just because you are offline.

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

On first start, Ferrogram asks for both on a setup screen. The **`api_hash`
is stored only in the OS keyring** (`dev.ferro.ferrogram` / `api-hash`). The
non-secret `api_id` goes to `~/.config/ferrogram/config.ini` (mode 0600).

Alternatively, set environment variables. `FERROGRAM_API_HASH` is imported
into the keyring on first use, so you only need it once:

```sh
export FERROGRAM_API_ID=123456
export FERROGRAM_API_HASH=0123456789abcdef0123456789abcdef
```

An `api_hash = …` line left in `config.ini` by an older version is moved into
the keyring and removed from the file. Never commit these values.

## Run

```sh
./target/release/ferrogram           # real account
./target/release/ferrogram --mock    # demo data, no network, nothing stored
```

`--mock` login codes: `12345` asks for the 2FA password `hunter2`; `22222`
signs in directly. In mock mode "Echo Bot" replies to whatever you send.

Environment variables: `FERRO_THEME=light`, `FERRO_RENDERER=wgpu` (GPU renderer; the default is the CPU renderer tiny-skia, ~30 MB idle), `FERRO_TRACE_STARTUP=1` (logs
`first_frame_ms`), `FERROGRAM_NOTIFY_SENDER_ONLY=1`, `FERROGRAM_NO_NOTIFY=1`,
`RUST_LOG=debug`.

Files:

* `~/.local/share/ferrogram/cache.sqlite3` is the message cache. It is
  **plain text** (SQLite), owner-only (directory 0700, files 0600), and safe
  to delete.
* `~/.local/share/ferrogram/session.enc` is the encrypted MTProto session.
  Its key lives in the keyring.
* `~/.local/state/ferrogram/log` is the log file, with phone numbers and
  bodies redacted.

A Secret Service provider (KWallet or GNOME Keyring) is required. Without one,
Ferrogram refuses to store the session or the API hash. If the session can't
be decrypted (for example, the keyring was reset), it is discarded and you
are asked to log in again.

`FERROGRAM_PROTOCOL_DEBUG=1` allows grammers' debug/trace logs, which contain
raw protocol data and are **not** redacted. Without it, those targets stay at
`warn` even with `RUST_LOG=trace`.

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
