# Protocol

## Library choice

Ferrogram talks MTProto through **`grammers-client` 0.12** (pure Rust,
Apache-2.0/MIT) with `grammers-session` (no default features, `serde` on).
No fallback to `tdlib-rs` was needed: login (phone → code → SRP 2FA),
dialogs, history paging, send text/media, edit, delete, reactions,
per-chat search, downloads and the ordered update stream are all available
in grammers. Raw TL types (`grammers_client::tl`) are used only in
`src/net/types.rs` for conversion and in tests.

`grammers-session`'s SQLite storage is **disabled** because it bundles
`libsql`, which would duplicate SQLite next to `rusqlite` and store the auth
key in plain text. Instead `net/session.rs` implements grammers' `Session`
trait in memory and persists it as an encrypted blob (see below).

## Client abstraction

```
app ──▶ net::TelegramApi (trait, async_trait)
          ├─ net::mtproto::Grammers   real account (grammers-client)
          └─ mock::FakeTelegram        tests and --mock mode
```

`TelegramApi` methods: `restore`, `request_code`, `sign_in`,
`check_password`, `persist_session`, `log_out`, `dialogs`, `history`,
`send_text`, `send_file`, `edit_message`, `delete_messages`, `react`,
`mark_read`, `search`, `download_photo`, `events`.

Round 2 added: `more_dialogs` (continue the dialog iterator), `search_global`
(`messages.searchGlobal`), `download_avatar`, `set_typing`, `save_draft`.

`events()` returns the live stream of `net::Event`. It is backed by
`net::EventHub`: a new subscription takes over from the previous one (whose
stream ends), and events emitted while nobody listens are kept in a small
bounded backlog
(`NewMessage`, `MessageEdited`, `MessagesDeleted`, `Reactions`,
`ReadOutbox`, `ReadInbox`, `Typing`, `UserStatus`, `Draft`, `Connection`,
`SessionRevoked`). The app
subscribes to it with `Subscription::run_with`.

## Identifiers

Chats use Bot-API style ids: users `id`, basic groups `-id`, channels and
supergroups `-1000000000000 - id` (`model::CHANNEL_OFFSET`). This makes one
`i64` unique across peer kinds and matches grammers'
`PeerId::bot_api_dialog_id()`. `updateDeleteMessages` carries no chat; it
is applied to every non-channel chat, as Telegram specifies.

## Conversion (`net/types.rs`)

| TL | Domain |
|---|---|
| `message` | `Message` (text, `out`, reply header → `reply_to`, `edit_date && !edit_hide` → `edited`, reactions) |
| `messageService` | `Message { service: true, text: <action label> }` |
| `messageMediaPhoto` | `Media::Photo { id, w, h }` (largest size) |
| `messageMediaDocument` | `Media::File { name, size, mime }`, `Sticker`, or labelled `Other` (voice, round video) |
| geo / venue / poll / contact / dice | `Media::Other { label }` |
| `updateNewMessage`, `updateNewChannelMessage` | `Event::NewMessage` |
| `updateEdit(Channel)Message` | `Event::MessageEdited` |
| `updateDelete(Channel)Messages` | `Event::MessagesDeleted` |
| `updateMessageReactions` | `Event::Reactions` |
| `updateRead(History|Channel)Outbox/Inbox` | `Event::ReadOutbox` / `ReadInbox` |
| `updateUserTyping`, `updateChatUserTyping`, `updateChannelUserTyping` | `Event::Typing { chat, user, action }` (`None` = cancel; forum topics and anonymous senders ignored) |
| `updateUserStatus`, `user.status` | `Event::UserStatus` / `Chat.presence` (a stale `online.expires` becomes "last seen") |
| `updateDraftMessage`, `dialog.draft` | `Event::Draft` / `Chat.draft` (empty text = no draft) |
| RPC errors | `rpc_error()` → `AuthError::{InvalidPhone, InvalidCode, InvalidPassword, FloodWait, SessionRevoked}` |

Fixtures in `tests/tl_conversion.rs` are serialised to MTProto bytes and
parsed back before conversion, so the deserialiser is exercised too.

## Round 2 features

* **Avatars.** `Chat.photo_id` comes from the dialog's peer (store migration
  3). `download_avatar` builds an `InputPeerPhotoFileLocation` (small, not
  big) and streams it with a 512 KB cap. Bytes go to
  `$XDG_CACHE_HOME/ferrogram/avatars/<chat>_<photo>.img` (owner-only, at most
  1500 files / 48 MB, a new photo id replaces the old file, wiped on logout)
  and are decoded off the UI thread into a circular RGBA image kept in a
  10 MB LRU. The disk cache is read before the network and works offline.
  Photos of users who are not a dialog partner (group senders) fall back to
  initials. Mock mode never writes to disk.
* **Typing / presence.** `TypingTracker` (6 s TTL, 256 entries) feeds the
  header and the chat-list preview. Our own action: `TypingThrottle` sends
  `messages.setTyping` at most every 5 s while the composer has text, and a
  cancel when it is emptied or the chat changes; never in edits, Saved
  Messages or channels; failures are only logged.
* **Files.** `Ui::OpenFile` → `media::is_save_only` (executables, scripts,
  launchers; last extension, trailing dots/spaces ignored) → otherwise
  `download_media` into `$XDG_CACHE_HOME/ferrogram/open/<chat>_<id>/<safe
  name>` (0600, sanitised name, 512 MB cap) → `open::that_detached` (no
  shell). `desktop::open_file` re-checks the extension.
* **Chat list paging.** `dialogs(100)` restarts, `more_dialogs(100)` continues
  the same `DialogIter`; the sidebar scroll triggers it 300 px before the end.
  The cache is pruned of vanished chats only when the first page was the
  whole list.
* **Global search.** 450 ms after the last keystroke `search_global` runs;
  hits are de-duplicated against the cache results. Opening a hit loads
  `history(before = id + 25)`, scrolls by an index estimate and highlights
  the message. That window is *detached*: live messages are not appended
  (no invisible gaps); the down-arrow button (or sending) reloads the newest
  page. Paging forward from a detached window is not supported.
* **Drafts.** The composer is synced 1 s after the last change, and on leaving
  the chat, through `messages.saveDraft` (store migration 4 keeps them
  locally). Edits are never drafts (the draft is stashed while editing).

## Not implemented (round 2)

Archived chats and folders (`DialogIter` cannot set `folder_id`; needs a raw
`messages.getDialogs` with our own peer conversion and peer caching), voice
playback (needs an Ogg/Opus decoder and an audio sink), video thumbnails,
sticker rendering and forwarding.

## Login

`auth::reduce(&mut AuthState, AuthEvent) -> Option<AuthCommand>` is pure.
States: `Restoring → Phone → Code → Password → SignedIn`. The SRP
computation for the 2FA password is done by grammers' `check_password`.
The password string is moved into a `SecretString` and cleared from the UI
state as soon as it is submitted.

## Credentials and session storage

* **API credentials** (`config::ApiCredentials::resolve`, a pure function
  over `CredentialInputs`, with each source injectable for tests). The first
  complete, well-formed pair wins; malformed pairs are skipped with a warning
  that never contains the values:
  1. user override: `config.ini` `api_id` plus the keyring `api-hash`, set
     from the *"Advanced: use your own API credentials"* login link or
     `--api-setup`;
  2. runtime `FERROGRAM_API_ID` / `FERROGRAM_API_HASH` (developers);
  3. values embedded at build time. `build.rs` reads the same env var names
     (`cargo:rerun-if-env-changed`), validates the id as a positive i32 and
     the hash as 32 hex characters (a malformed value fails the build), and
     emits `FERROGRAM_EMBEDDED_API_*` for `option_env!`. The release
     workflow fills them from the repository secrets `TELEGRAM_API_ID` /
     `TELEGRAM_API_HASH`.

  With embedded values the first screen is the phone screen, as in the
  official client. Without any source (a development build), the setup
  screen is shown and cannot be skipped. Switching credentials shuts down the
  old client, clearing its unauthorised session, and starts a new one. A
  legacy `api_hash` line in `config.ini` is migrated to the keyring. The
  `api_hash` identifies the app, is extractable from any distributed binary,
  and is not a user secret; the user's own override is still kept out of
  plain files.
* The grammers session (auth keys per DC, peer access hashes, update
  state) is serialised to JSON, encrypted with ChaCha20-Poly1305 using a
  random 256-bit key stored in the OS keyring (`dev.ferro.ferrogram` /
  `session-key`), and written to `$XDG_DATA_HOME/ferrogram/session.enc`
  (mode 0600). It is saved after login, and every 20 s if dirty.
* If no keyring is available the app shows an error on the login screen
  and never falls back to a plain file.
* If the blob cannot be decrypted or parsed (lost or reset keyring key,
  corrupt file), it is deleted, a fresh session is started and a toast asks
  the user to log in again. Keyring *access* errors are reported instead,
  because the blob may still be fine.
* Logout and revoke call `EncryptedSession::clear()`, which marks the session
  closed: the periodic flusher stops, and no late save can write the old key
  back. The app then builds a **new** `GrammersApi`, with a new SenderPool,
  update receiver and event hub. Nothing from the old session is reused.
* The SQLite cache is plain text, but owner-only (directory 0700, files
  0600, including `-wal` and `-shm`).

## History contiguity

`history_ranges(chat_id, lo, hi)` (store migration 2) records the id range
that the *network* has proven complete in the cache:

* A newest page (`before = None`) proves `[min, max]` of the page.
* A page `before = b` proves `[min, b - 1]`.
* A short page proves everything down to the start (`lo = 0`).
* Ranges are unioned when they touch or overlap. A disjoint newer range
  replaces the old one, because the old one can no longer be proven
  contiguous with the present.

`load_older_cached(chat, before)` serves a page from the cache only if
`lo < before <= hi + 1` and the page is full (or `lo == 0`). Otherwise it
returns nothing and the app asks the network. Example: an old cache of 1–50
and a fresh network page 901–950 give range `[901, 950]`, so paging before 901
goes to the network instead of jumping straight to message 50.

## Offline start

`restore()` failing with a transient error (`Net`, `Cancelled`) keeps the app
in `Restoring`. The cached chat list stays on screen with a "Waiting for
network…" banner, and the restore is retried with the same exponential
backoff. Only a definite `Ok(None)` (not authorised) or a revoked session
shows the phone screen. Per-request failures show a toast and never change
the connection state.

## Reconnects

grammers' sender pool reconnects requests transparently. The update loop
treats stream errors as `Reconnecting { attempt }` and backs off
exponentially with jitter (`net/backoff.rs`, 1 s → 60 s). Session-revoked
RPC errors clear the vault and the cache and return to the login screen.

## Not verified against real servers

No real account exists in the build environment, so the grammers path
(`net/mtproto.rs`) is compiled and type-checked but has not been exercised
live. Everything above it is tested through `FakeTelegram`.
