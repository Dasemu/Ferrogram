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

Calls added: `call_config`, `dh_config`, `request_call`, `received_call`,
`accept_call`, `confirm_call`, `discard_call`, `send_call_signaling`
(defaults return "unsupported").

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

## Round 3 features

* **Chat folders.** `messages.getDialogFilters`, `messages.updateDialogFilter`
  (create/replace; no filter = delete) and `messages.updateDialogFiltersOrder`
  through raw `invoke`; the TL structs stay in `net/types.rs`
  (`folder_from_tl` / `folder_to_tl`). The access hashes of the peers a folder
  names are kept (`folder_peers`) so a folder can be saved again for chats
  outside the loaded list; saving fails loudly (never drops a chat) when a
  peer cannot be resolved. `updateDialogFilter`, `updateDialogFilterOrder`
  and `updateDialogFilters` become `Event::FolderChanged / FoldersOrder /
  FoldersStale`. Membership is evaluated locally with Telegram Desktop's
  rules (`Folder::matches`: never list, explicit chats, then type flags
  narrowed by exclude muted/read/archived). `chat.contact` comes from the
  user object (migration 5); folders are cached as JSON in `kv`. Shared
  (`dialogFilterChatlist`) folders are listed and can be deleted/reordered
  but not edited. Archived chats are not loaded, so "exclude archived" is a
  no-op for now. A folder shows only loaded dialogs; while it shows few and
  the list is not exhausted, further dialog pages are loaded.
* **Clipboard.** `clipboard.rs` (`arboard`, features `image-data` and
  `wayland-data-control`): Ctrl+V (physical V key, delivered even when the
  composer captured it) reads copied files or an image; images are written
  as PNG to a private temp file (`$XDG_CACHE_HOME/ferrogram/paste`), then
  take the normal send-preview path; files that file managers also offer as
  text are removed from the composer again. "Copy image" decodes the cached
  or downloaded photo (header checks, pixel cap) and sets it through one
  long-lived handle (X11 must keep serving it).
* **Info panel.** `users.getFullUser`, `messages.getFullChat`,
  `channels.getFullChannel` give bio/description, username, phone (only when
  Telegram returns it) and member count; the Notifications switch uses
  `account.updateNotifySettings` (mute forever = `i32::MAX`) and follows
  `updateNotifySettings`. Shared tabs use `messages.search` with
  `inputMessagesFilterPhotoVideo / Document / Url / Music / Voice / Gif`,
  40 per page, `offset_id` = oldest id of the previous page, at most 600
  items per tab. Previews (320 px "m" size) are downloaded 4 at a time from
  a bounded queue into a 16 MB LRU that is dropped when the panel closes or
  the chat changes (round 4: only rows near the viewport are fetched, and the
  previews are decoded and cropped to the drawn size; see "Round 4"). Photos open the viewer, other items jump to their
  message, links open in the browser (`desktop::open_url`: http(s) only).

## Round 4 features

**Notification settings** (`model::NotifySettings`, `NotifyDefaults`).
`peerNotifySettings` is read from every `Dialog` (`mute_until`, `silent`,
`show_previews`; unset stays unset) and from `updateNotifySettings`
(`Event::Notify`, target = a chat or `notifyUsers/Chats/Broadcasts`). The
account defaults come from `account.getNotifySettings` for
`inputNotifyUsers` (private chats, bots), `inputNotifyChats` (groups and
supergroups) and `inputNotifyBroadcasts` (channels), fetched after every
sign-in. `NotifyDefaults::resolve` gives the effective state: the chat's own
field if set, else the default of its kind, else loud with previews; Saved
Messages is never muted. `Chat::muted` is that result, recomputed by
`ChatList` when settings, defaults or the clock change (a 30 s tick expires
timed mutes). A muted chat: no OS notification (`should_notify`), grey
badge, left out of the tray unread count and attention state. `silent`
shows the notification without sound (`SuppressSound` hint), `show_previews
= false` shows "New message". Muting here calls `account.updateNotifySettings`
with only `mute_until` (explicit `0` to unmute, which beats a "muted by
default" setting) and is optimistic with revert. Cached in `chats`
(`notify_*` columns, migration 6) and `kv` (`notify_defaults`).

**In-chat search** (`app/chatmenu.rs::ChatSearch`). `messages.search` with
the peer, 50 per page, `offset_id` = oldest id so far, at most 500 hits;
typing searches after 350 ms, the arrows and Enter walk the hits (newest
first; up = older) and jump to each with the existing window/highlight
machinery; stepping past a full page fetches the next. The "from:" filter
(`from_id`, groups only) lists members from `messages.getFullChat` (basic
groups) or `channels.getParticipants` (recent 200); access hashes come from
the same answers. Answers carry a version so a late page for an old query is
ignored.

**⋮ menu** (`menu_items` decides the entries per chat kind): View
profile/info (toggles the panel), Mute for 1 h / 8 h / 1 day / 1 week /
forever or Unmute, Search, Select messages (tick, Copy, Delete with
confirmation), Add to folder, Clear history (`messages.deleteHistory`
`just_clear`, repeated while `offset > 0`; basic groups and private chats
only: clearing a supergroup needs admin rights we do not know), Delete chat
(`messages.deleteHistory`), Leave group (`messages.deleteChatUser` + delete
the dialog) / Leave channel (`channels.leaveChannel`), Block/Unblock
(`contacts.block` / `contacts.unblock`; the state comes from
`userFull.blocked`). Destructive entries ask first; private chats offer
"Also delete for X" (`revoke`).

**Media grid.** Fixed-height rows (`app::info::ROW_H`) let the view build
only the rows around the viewport (scroll offset from `on_scroll`, spacers
for the rest); only their previews are requested. Previews are decoded and
centre-cropped on `spawn_blocking` to a square of exactly the drawn size
(`CELL` x the window scale factor) and cached as RGBA handles.

## Voice calls (one-to-one)

Feasibility, evidence and the choice of protocol: `docs/calls-feasibility.md`.
In short: the legacy libtgvoip protocol (2.4.4) is no longer registered by
any official app, so Ferrogram speaks tgcalls V2 and advertises
`library_versions = ["9.0.0", "8.0.0"]`, `min_layer 65`, `max_layer 92`,
`udp_p2p`, `udp_reflector`. The server picks one version for both sides
(`phoneCall.protocol.library_versions[0]`); anything else ends the call with
"incompatible call version".

**Signaling (`app/call.rs`, pure reducer).** Outgoing: `messages.getDhConfig`
(random_length 256, `NotModified` reuses the cached p/g) → check p (2048-bit
safe prime, Miller-Rabin on p and (p-1)/2, cached per process) and g (the
quadratic-residue conditions) → `a` = random XOR server random, `g_a`
checked to lie in `[2^1984, p - 2^1984]` → `phone.requestCall(g_a_hash =
SHA-256(g_a))` → `phoneCallWaiting` (Ringing once `receive_date` is set) →
`phoneCallAccepted(g_b)` → key `g_b^a mod p`, `key_fingerprint` = low 64
bits of SHA-1(key) → `phone.confirmCall(g_a, fingerprint)` → `phoneCall`
(connections). Incoming: `phoneCallRequested` → `phone.receivedCall`,
notification, tray attention, window shown → Accept → `phone.acceptCall(g_b)`
→ `phoneCall(g_a)`: check SHA-256(g_a) against the hash, derive the key and
compare fingerprints. A second incoming call while busy is discarded with
`Busy`. Hang-up reasons follow Telegram Desktop: cancelling an outgoing call
before it is answered = `Missed`, declining = `Busy`, otherwise `Hangup` with
the connected duration; the caller gives up after `call_ring_timeout_ms`
(`phone.getCallConfig`, default 90 s). Updates that beat the RPC reply (an
`Accepted` before `requestCall` returned) are kept until the DH half exists.

**Key verification.** Four emoji from SHA-256(key || g_a): four big-endian
64-bit chunks with the top bit cleared, each mod 333 into the protocol's
emoji table (`net/calls/emoji.rs`), shown in the call panel with the
official explanation.

**Signaling channel (`net/calls/signaling.rs`).** JSON messages
(`InitialSetup`, `NegotiateChannels`, `Candidates`, `MediaState`) inside the
V2 framing (sequence numbers, acks, resend after 3 s), encrypted with the
call key: `msg_key` = bytes 8..24 of SHA-256(key[88+x..120+x] || plaintext),
MTProto 2.0 KDF, AES-256-CTR, x = 128 (+8 for packets from the callee).
Sent with `phone.sendSignalingData`, received as
`updatePhoneCallSignalingData`; early packets are buffered (64).

**Transport (`net/calls/ice.rs`, `engine.rs`).** Own sans-IO ICE: host
candidates on the default-route IPv4/IPv6 addresses (when `p2p_allowed`),
relay candidates on Telegram reflectors (`phoneConnection`, UDP, 16-byte
peer tag; numbered by their position among all reflector ids, sorted;
announced as `reflector-<n>-<tag>.reflector`; "standalone" mode from
`custom_parameters` supported). The caller is ICE-controlling with
aggressive nomination and the DTLS client (`setup actpass`, callee
`passive`); webrtc-dtls 0.12 with an ECDSA certificate whose SHA-256
fingerprint is pinned through `InitialSetup`; SRTP `AES_CM_128_HMAC_SHA1_80`
from the DTLS exporter (`net/calls/srtp.rs`). Content negotiation mirrors
tgcalls: each side offers its audio stream (Opus PT 111, its SSRC), the
other answers; on a race the caller wins.

**Media (`net/calls/audio.rs`, `jitter.rs`).** Opus 48 kHz mono, 20 ms,
32 kbit/s with in-band FEC; adaptive jitter buffer (2–10 frames, 50 max,
FEC for single losses, PLC otherwise); cpal capture/playback on their own
thread, linear resampling to the device rate; mute sends silence (the stream
keeps running). Signal bars come from the loss rate per second. **No echo
cancellation yet**: with loudspeakers the peer can hear itself; headphones
avoid it.

**Lifetime.** Nothing call-related exists until a call starts: the engine
task, its sockets and the audio thread are created on `StartEngine` and
dropped (off the UI thread) when the call ends. Engine events reach the app
through a per-call `Subscription::run_with`.

**Not done / limits.** No SCTP data channel (remote mute/battery state is not
shown, and the peer does not see that we muted); no TURN/STUN from
`phoneConnectionWebrtc` and no TCP reflectors (UDP-blocking networks will not
connect); no video; no AEC/noise suppression; no `phone.setCallRating`;
`network_use_mtproto` (an alternative media transport the server can ask
for) is not implemented. Verified against our own engine and a fake
reflector only; a call to an official app needs a real account.

## Not implemented (round 2)

Round 4 leaves out "Export chat" (needs the desktop-only export flow),
forwarding selected messages, per-chat sound/preview overrides in the UI
(read and honoured, not editable) and a disk cache for shared-media previews.

Archived chats (`DialogIter` cannot set `folder_id`; needs a raw
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
