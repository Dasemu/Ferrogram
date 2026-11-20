# Voice calls: feasibility

Question: can Ferrogram place and receive one-to-one Telegram voice calls with
a pure-Rust media stack, so that calls work with the current official apps
(Telegram Desktop, Android, iOS)?

**Verdict: yes, with option (b), tgcalls "V2" (library versions 8.0.0 and
9.0.0).** Option (a), the legacy libtgvoip protocol, is no longer accepted by
any official client, so it is not an option.

## How a version gets picked

Both sides put a `phoneCallProtocol` with `library_versions` into
`phone.requestCall` / `phone.acceptCall`. The server reconciles the two lists
and returns **one** version in the final `phoneCall.protocol`; both clients
use `library_versions[0]`:

* Telegram Desktop `calls_call.cpp`: `vlibrary_versions().v.value(0,
  kDefaultVersion)` then `tgcalls::Meta::Create(version, ...)`; an unknown
  version ends the call with "Wrong library version".
* Telegram iOS `OngoingCallThreadLocalContext.mm`: "the server reconciles
  both endpoints' lists and returns one version, so no client-side gating is
  needed."

So a third-party client only needs to advertise a subset that the official
apps also support. If the intersection is empty the call fails.

## What the official apps advertise (checked 2026-11-20)

| Client | Registered tgcalls implementations | Versions |
|---|---|---|
| Telegram Desktop (`dev`, `calls_call.cpp`) | `InstanceImpl`, `InstanceV2Impl`, `InstanceV2ReferenceImpl` | 2.7.7, 5.0.0, 7.0.0, 8.0.0, 9.0.0, 12.0.0, 13.0.0, 10.0.0, 11.0.0 |
| Telegram Android (`org_telegram_messenger_voip_Instance.cpp`) | `InstanceImpl`, `InstanceV2Impl`, `InstanceV2ReferenceImpl`, `InstanceV2PumpImpl` | same plus 18/19 |
| Telegram iOS (`OngoingCallThreadLocalContext.mm`) | `InstanceImpl`, `InstanceV2Impl`, `InstanceV2ReferenceImpl`, `InstanceV2CompatImpl`, `InstanceV2PumpImpl` | same plus 14.0.0, 18.0.0, 19.0.0 |

Version lists come from tgcalls `master` (`InstanceImpl::GetVersions` = 2.7.7,
5.0.0; `InstanceV2Impl::GetVersions` = 7.0.0, 8.0.0, 9.0.0, 12.0.0, 13.0.0;
`InstanceV2ReferenceImpl` = 10.0.0, 11.0.0; `InstanceV2CompatImpl` = 14.0.0).
`tgcalls/legacy/` (the libtgvoip wrapper, version 2.4.4) no longer exists
in tgcalls, and none of the three apps registers `InstanceImplLegacy`.
Telegram Desktop still has `kDefaultVersion = "2.4.4"` only as the fallback
string when the list is empty.

## Option (a): legacy libtgvoip (2.4.4 / 2.7)

* Its own UDP/TCP protocol through Telegram reflectors, MTProto-2.0-style
  packet encryption with the call key, Opus. libtgvoip is public domain
  (Unlicense), so studying or porting it would be easy legally.
* **Not workable:** no current official client registers 2.4.4, so the
  version intersection with any up-to-date peer is empty and the call fails
  before media starts. (2.7.7 is *not* libtgvoip: it is the first WebRTC
  `InstanceImpl`.)

## Option (b): tgcalls WebRTC protocol

`InstanceV2Impl` (7.0.0, 8.0.0, 9.0.0, 12.0.0, 13.0.0), the version every
official app supports. What it needs on the wire:

1. **Signaling** through `phone.sendSignalingData` /
   `updatePhoneCallSignalingData`. JSON messages (`InitialSetup` with ICE
   ufrag/pwd and the DTLS fingerprint, `NegotiateChannels` offer/answer with
   SSRC, payload types and RTP extensions, `Candidates` with SDP candidate
   lines, `MediaState`). They are encrypted with the call key like MTProto
   2.0 (`msg_key` = middle 16 bytes of SHA-256 over key[88+x..120+x] and the
   plaintext, AES-256-CTR, x = 128 for signaling, +8 for the callee's
   direction).
   * 7.0.0 = signaling V1 (one raw message per packet),
   * 8.0.0 / 9.0.0 = V2 (the same encryption plus message framing with
     sequence numbers, acks and resends),
   * 12.0.0 / 13.0.0 = V3 (gzip plus signaling over an SCTP association).
2. **Transport**: standard ICE (caller is controlling), DTLS 1.2 with
   ECDSA certificates (caller is the DTLS client, callee the server), and
   DTLS-SRTP for media. Relays are:
   * `phoneConnection` reflectors, used as relay candidates with a small
     custom framing (16-byte peer tag, sender tag, length, payload; candidate
     host names `reflector-<server>-<tag>.reflector`); optional
     "standalone" mode with fixed role tags 1/2;
   * `phoneConnectionWebrtc` entries: plain STUN and TURN servers.
3. **Media**: Opus (48 kHz) in RTP over SRTP. An SCTP data channel carries
   `MediaState` (remote mute, battery), but the connection state only
   depends on ICE + DTLS being writable (`NativeNetworkingImpl::
   UpdateAggregateStates_n`), so audio works without it.

**Evidence that a third-party implementation interoperates:** ntgcalls
(the C++ engine behind pytgcalls, LGPL-3.0) implements private calls with
exactly this protocol (it advertises 8.0.0, 9.0.0, 12.0.0, 13.0.0, with its
own reflector port) and is used to call official apps. It is built on
libwebrtc, not on Rust, but it shows that the protocol is complete as
described above and that the server accepts it from third-party clients.

### Pure-Rust building blocks

| Piece | Choice |
|---|---|
| ICE + reflector relay | Own small ICE agent (sans-IO, STUN codec, host + reflector candidates, aggressive nomination as controller). Needed because no Rust ICE crate can host the custom reflector relay. |
| DTLS 1.2 + `use_srtp` | `webrtc-dtls` 0.12 (webrtc-rs, MIT/Apache; interoperates with libwebrtc in browsers). |
| SRTP | Own `AES_CM_128_HMAC_SHA1_80` (RFC 3711, tested against the RFC vectors); libwebrtc always offers this suite. |
| Signaling crypto | `aes`, `ctr`, `sha2` (already in the tree through grammers). |
| DH, fingerprint, emoji | `num-bigint` (already in the tree), `sha1`, `sha2`. |
| Codec | `opus` (libopus). |
| Audio I/O | `cpal` (ALSA; PipeWire/PulseAudio through their ALSA plugins). |

webrtc-rs's full `RTCPeerConnection` was considered, but Telegram does not
exchange SDP and the reflector relay cannot be plugged into its ICE agent;
using it would lose the relay that Telegram provides for every call.

### Chosen path

* Advertise `library_versions = ["9.0.0", "8.0.0"]`, `min_layer = 65`,
  `max_layer = 92`, `udp_p2p` and `udp_reflector`, like Telegram Desktop.
  V2 signaling was picked over V1 (7.0.0) because it is what ntgcalls uses
  too (so it can be tested against a real libwebrtc peer) and over V3
  because V3 needs an SCTP stack for signaling.
* Signaling V2, own ICE with host candidates, UDP reflectors (normal and
  standalone mode), webrtc-dtls, own SRTP, Opus mono 48 kHz, 20 ms frames.
* No SCTP data channel at first: remote mute is not shown, and our mute is
  not shown to the peer (the peer just hears silence). Documented.

### Effort

About 6-8k lines: net layer (`phone.*`, updates, DH) ~1k, signaling ~800,
ICE/STUN/reflector ~1.5k, SRTP/RTP ~600, engine ~800, audio (cpal, Opus,
jitter buffer) ~800, UI ~1k, tests ~1.5k.

### Risks

* **No real peer in CI.** The protocol is verified against a local
  libwebrtc peer where possible (ntgcalls wheel, see the PR), but the first
  real call to a phone needs a real account.
* **Server `custom_parameters`.** The server can switch on transport
  variants per call (`network_use_mtproto` replaces DTLS with an MTProto
  RTP transport; ntgcalls does not implement it either). Ferrogram logs it
  and keeps DTLS; such a call would not connect.
* **TCP reflectors and TURN** are not used at first (UDP reflectors are);
  networks that block UDP entirely will not connect.
* **No echo cancellation**: use headphones; documented in the UI notes.
* **Version churn**: if Telegram drops 8.0.0/9.0.0 from the official apps,
  calls fail with "incompatible version" until V3 (12.0.0+) is added.
* **Licences.** tgcalls (LGPL-3.0), Telegram Desktop (GPL-3.0) and ntgcalls
  (LGPL-3.0) were read as protocol references only; no code was copied. The
  emoji table for the key fingerprint is the protocol's interop constant (the
  list of 333 emoji and their order) and is reproduced as data.

## Sources

* tgcalls: https://github.com/TelegramMessenger/tgcalls (`tgcalls/v2/`,
  `EncryptedConnection.cpp`, `CryptoHelper.cpp`)
* Telegram Desktop: `Telegram/SourceFiles/calls/calls_call.cpp`,
  `calls_emoji_fingerprint.cpp`
* Telegram Android: `TMessagesProj/jni/voip/org_telegram_messenger_voip_Instance.cpp`
* Telegram iOS: `submodules/TgVoipWebrtc/Sources/OngoingCallThreadLocalContext.mm`
* ntgcalls: https://github.com/pytgcalls/ntgcalls (`ntgcalls/src/signaling/signaling.cpp`)
* libtgvoip: https://github.com/grishka/libtgvoip (last commit 2019)
* API docs: https://core.telegram.org/api/end-to-end/voice-calls (not
  reachable from the build environment; the DH checks follow the MTProto
  rules that page references)
