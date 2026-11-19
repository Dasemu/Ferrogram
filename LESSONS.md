# Lessons

Things learned while building Ferrogram, newest first.

- **Take-once channels break re-login.** An event receiver or update pump
  taken on first use silently stops delivering events after logout → login.
  Make subscriptions restartable (`EventHub`), and rebuild the protocol
  client on logout/revoke.
- **Offline ≠ logged out.** Treating a network error from `restore()` as
  "show the phone screen" costs a fresh SMS, risks FLOOD_WAIT and can
  overwrite a good session. Retry transient errors with backoff and keep the
  cache visible.
- **A cache is not a history.** Serving "older" pages from SQLite without
  knowing that the range is contiguous hides gaps. Track network-confirmed
  ranges per chat.
- **`tokio::time::sleep()` needs a runtime when called, not when awaited.**
  Wrap it in `async { … }` inside `Task::perform`, or unit tests that drive
  `update()` without a runtime panic.
- **Notification bodies are markup** on KDE/freedesktop. Escape `& < >` in
  both title and body, and coalesce bursts (catch-up after a reconnect).
- **Check image headers before decoding** and cap bytes and pixels, so a
  hostile "photo" cannot exhaust memory.

- **grammers-session's SQLite storage clashes with rusqlite.** Its default
  `sqlite-storage` feature pulls `libsql` (another SQLite) and writes the auth
  key unencrypted. Disable default features and implement `Session` yourself;
  ~150 lines (`net/session.rs`) with serde support from the `serde` feature.
- **grammers `Dialog::peer_ref()` can panic** (`auth().unwrap()`), and
  `Peer::auth()` is crate-private. Use `peer().to_ref().await`.
- **No need for tdlib-rs.** grammers 0.12 covers login with SRP, dialogs,
  history, media upload/download, edits, deletes, reactions, search and an
  ordered update stream with gap recovery.
- **TL fixtures are cheap to make "recorded"**: build the struct, call
  `to_bytes()`, then `from_bytes()` — the deserialiser runs as if the bytes
  came off the wire.
- **iced 0.14 layout:** a `container` whose child row contains a `Fill`
  space does not always expand; set `.width(Length::Fill)` explicitly or the
  background only covers part of the bar.
- **Colour emoji:** cosmic-text's fallback may pick a monochrome font for
  emoji. Split text into emoji/non-emoji runs and give emoji spans the
  `Noto Color Emoji` font via `rich_text`.
- **No ellipsis in iced text.** Truncate by chars and set
  `Wrapping::None` + `container.clip(true)` for single-line previews.
- **Redaction writer vs timestamps:** a naive "mask digit runs with dashes"
  rule also masked ISO dates in log lines. Only accept separators after a
  leading `+`.
- **Deterministic screenshots:** pin "now" (`mock::MOCK_NOW`) and the time
  zone offset in mock mode, and drive the UI with a timed message script.
