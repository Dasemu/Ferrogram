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

`events()` returns the live stream of `net::Event`
(`NewMessage`, `MessageEdited`, `MessagesDeleted`, `Reactions`,
`ReadOutbox`, `ReadInbox`, `Connection`, `SessionRevoked`). The app
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
| RPC errors | `rpc_error()` → `AuthError::{InvalidPhone, InvalidCode, InvalidPassword, FloodWait, SessionRevoked}` |

Fixtures in `tests/tl_conversion.rs` are serialised to MTProto bytes and
parsed back before conversion, so the deserialiser is exercised too.

## Login

`auth::reduce(&mut AuthState, AuthEvent) -> Option<AuthCommand>` is pure.
States: `Restoring → Phone → Code → Password → SignedIn`. The SRP
computation for the 2FA password is done by grammers' `check_password`.
The password string is moved into a `SecretString` and cleared from the UI
state as soon as it is submitted.

## Credentials and session storage

* `api_id` / `api_hash` come from `FERROGRAM_API_ID` / `FERROGRAM_API_HASH`
  or `$XDG_CONFIG_HOME/ferrogram/config.ini`; never compiled in.
* The grammers session (auth keys per DC, peer access hashes, update
  state) is serialised to JSON, encrypted with ChaCha20-Poly1305 using a
  random 256-bit key stored in the OS keyring (`dev.ferro.ferrogram` /
  `session-key`), and written to `$XDG_DATA_HOME/ferrogram/session.enc`
  (mode 0600). It is saved after login, and every 20 s if dirty.
* If no keyring is available the app shows an error on the login screen
  and never falls back to a plain file.

## Reconnects

grammers' sender pool reconnects requests transparently. The update loop
treats stream errors as `Reconnecting { attempt }` and backs off
exponentially with jitter (`net/backoff.rs`, 1 s → 60 s). Session-revoked
RPC errors clear the vault and the cache and return to the login screen.

## Not verified against real servers

No real account exists in the build environment, so the grammers path
(`net/mtproto.rs`) is compiled and type-checked but has not been exercised
live. Everything above it is tested through `FakeTelegram`.
