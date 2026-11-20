//! One-to-one voice calls (tgcalls protocol V2, library versions 8.0.0 and
//! 9.0.0). See `docs/calls-feasibility.md` and `docs/protocol.md`.
//!
//! Nothing here runs until a call starts: the engine, its sockets and the
//! audio threads are created per call and dropped when it ends.
//!
//! Domain types here carry no TL; conversion lives in `net/types.rs`.

pub mod audio;
pub mod crypto;
pub mod dh;
pub mod emoji;
pub mod engine;
pub mod ice;
pub mod jitter;
pub mod rtp;
pub mod signaling;
pub mod srtp;
pub mod stun;

use std::fmt;

/// What we advertise in `phoneCallProtocol` (see the feasibility study).
pub const LIBRARY_VERSIONS: [&str; 2] = ["9.0.0", "8.0.0"];
pub const MIN_LAYER: i32 = 65;
pub const MAX_LAYER: i32 = 92;

/// `inputPhoneCall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallRef {
    pub id: i64,
    pub access_hash: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protocol {
    pub udp_p2p: bool,
    pub udp_reflector: bool,
    pub min_layer: i32,
    pub max_layer: i32,
    pub library_versions: Vec<String>,
}

impl Protocol {
    /// Ours, as sent in `requestCall` / `acceptCall` / `confirmCall`.
    pub fn ours() -> Self {
        Self {
            udp_p2p: true,
            udp_reflector: true,
            min_layer: MIN_LAYER,
            max_layer: MAX_LAYER,
            library_versions: LIBRARY_VERSIONS.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// The version the server picked (first entry), if we speak it.
    pub fn chosen_supported(&self) -> Option<&str> {
        let v = self.library_versions.first()?;
        LIBRARY_VERSIONS.contains(&v.as_str()).then_some(v.as_str())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Connection {
    /// A Telegram reflector (`phoneConnection`).
    Reflector {
        id: i64,
        ip: String,
        ipv6: String,
        port: i32,
        peer_tag: Vec<u8>,
        tcp: bool,
    },
    /// STUN / TURN (`phoneConnectionWebrtc`).
    Webrtc {
        id: i64,
        ip: String,
        ipv6: String,
        port: i32,
        turn: bool,
        stun: bool,
        username: String,
        password: String,
    },
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reflector { id, port, tcp, .. } => f
                .debug_struct("Reflector")
                .field("id", id)
                .field("port", port)
                .field("tcp", tcp)
                .finish_non_exhaustive(),
            Self::Webrtc { id, turn, stun, .. } => f
                .debug_struct("Webrtc")
                .field("id", id)
                .field("turn", turn)
                .field("stun", stun)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardReason {
    Missed,
    Disconnect,
    Hangup,
    Busy,
    /// Anything newer (e.g. moved to a group call).
    Other,
}

/// `PhoneCall` without TL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneCall {
    Empty {
        id: i64,
    },
    Waiting {
        call: CallRef,
        admin_id: i64,
        participant_id: i64,
        video: bool,
        /// Set once the callee's device got the call (it is ringing).
        receive_date: Option<i32>,
    },
    Requested {
        call: CallRef,
        admin_id: i64,
        participant_id: i64,
        video: bool,
        g_a_hash: Vec<u8>,
        protocol: Protocol,
    },
    Accepted {
        call: CallRef,
        admin_id: i64,
        participant_id: i64,
        g_b: Vec<u8>,
        protocol: Protocol,
    },
    Active {
        call: CallRef,
        admin_id: i64,
        participant_id: i64,
        g_a_or_b: Vec<u8>,
        key_fingerprint: i64,
        protocol: Protocol,
        connections: Vec<Connection>,
        p2p_allowed: bool,
        start_date: i32,
        custom_parameters: Option<String>,
    },
    Discarded {
        id: i64,
        reason: Option<DiscardReason>,
        duration: Option<i32>,
        need_rating: bool,
    },
}

impl PhoneCall {
    pub fn id(&self) -> i64 {
        match self {
            Self::Empty { id } | Self::Discarded { id, .. } => *id,
            Self::Waiting { call, .. }
            | Self::Requested { call, .. }
            | Self::Accepted { call, .. }
            | Self::Active { call, .. } => call.id,
        }
    }
}

/// `phone.getCallConfig` values we use (milliseconds in the JSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallConfig {
    pub ring_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub packet_timeout_ms: u64,
    pub receive_timeout_ms: u64,
}

impl Default for CallConfig {
    /// Telegram Desktop's defaults.
    fn default() -> Self {
        Self {
            ring_timeout_ms: 90_000,
            connect_timeout_ms: 30_000,
            packet_timeout_ms: 10_000,
            receive_timeout_ms: 20_000,
        }
    }
}

impl CallConfig {
    /// Parse the `phone.getCallConfig` JSON, keeping defaults for anything
    /// missing or out of range.
    pub fn from_json(s: &str) -> Self {
        let mut c = Self::default();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
            return c;
        };
        let get = |k: &str, d: u64| {
            v.get(k)
                .and_then(serde_json::Value::as_u64)
                .filter(|x| (1000..=600_000).contains(x))
                .unwrap_or(d)
        };
        c.ring_timeout_ms = get("call_ring_timeout_ms", c.ring_timeout_ms);
        c.connect_timeout_ms = get("call_connect_timeout_ms", c.connect_timeout_ms);
        c.packet_timeout_ms = get("call_packet_timeout_ms", c.packet_timeout_ms);
        c.receive_timeout_ms = get("call_receive_timeout_ms", c.receive_timeout_ms);
        c
    }
}

/// Reflectors usable by the engine: UDP `phoneConnection`s with a 16-byte
/// tag. The server id is the 1-based position of the connection id among
/// all reflector ids, sorted (as the official apps number them).
pub fn reflectors(connections: &[Connection]) -> Vec<ice::ReflectorConfig> {
    let mut ids: Vec<i64> = connections
        .iter()
        .filter_map(|c| match c {
            Connection::Reflector { id, .. } => Some(*id),
            Connection::Webrtc { .. } => None,
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out = Vec::new();
    for c in connections {
        let Connection::Reflector {
            id,
            ip,
            port,
            peer_tag,
            tcp,
            ..
        } = c
        else {
            continue;
        };
        if *tcp || peer_tag.len() != 16 {
            continue;
        }
        let Ok(port) = u16::try_from(*port) else {
            continue;
        };
        let Ok(ip) = ip.parse::<std::net::Ipv4Addr>() else {
            continue;
        };
        let Some(pos) = ids.iter().position(|x| x == id) else {
            continue;
        };
        let Ok(server) = u8::try_from(pos + 1) else {
            continue;
        };
        let mut tag = [0u8; 16];
        tag.copy_from_slice(peer_tag);
        out.push(ice::ReflectorConfig {
            server,
            addr: std::net::SocketAddr::new(ip.into(), port),
            peer_tag: tag,
        });
    }
    out.truncate(16);
    out
}

/// A boolean flag from the call's `custom_parameters` JSON.
pub fn custom_flag(custom: Option<&str>, name: &str) -> bool {
    custom
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get(name).cloned())
        .is_some_and(|v| v.as_bool().unwrap_or(false) || v.as_i64() == Some(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refl(id: i64, ip: &str, tcp: bool, tag_len: usize) -> Connection {
        Connection::Reflector {
            id,
            ip: ip.into(),
            ipv6: String::new(),
            port: 599,
            peer_tag: vec![1; tag_len],
            tcp,
        }
    }

    #[test]
    fn reflector_numbering_matches_official_apps() {
        let conns = vec![
            refl(900, "149.154.1.3", false, 16),
            Connection::Webrtc {
                id: 1,
                ip: "1.1.1.1".into(),
                ipv6: String::new(),
                port: 3478,
                turn: true,
                stun: true,
                username: "u".into(),
                password: "p".into(),
            },
            refl(100, "149.154.1.1", true, 16), // TCP: numbered, not used
            refl(500, "149.154.1.2", false, 16),
            refl(700, "not-an-ip", false, 16),
            refl(800, "149.154.1.4", false, 8), // bad tag
        ];
        let r = reflectors(&conns);
        let got: Vec<(u8, String)> = r.iter().map(|c| (c.server, c.addr.to_string())).collect();
        assert_eq!(
            got,
            vec![
                (5, "149.154.1.3:599".to_owned()),
                (2, "149.154.1.2:599".to_owned())
            ]
        );
        assert!(!format!("{:?}", conns[1]).contains("\"p\""));
    }

    #[test]
    fn protocol_and_config() {
        let mut p = Protocol::ours();
        assert_eq!(p.chosen_supported(), Some("9.0.0"));
        p.library_versions = vec!["11.0.0".into()];
        assert_eq!(p.chosen_supported(), None);
        let c = CallConfig::from_json(r#"{"call_connect_timeout_ms":15000,"call_packet_timeout_ms":5,"x":1}"#);
        assert_eq!(c.connect_timeout_ms, 15_000);
        assert_eq!(c.packet_timeout_ms, 10_000);
        assert_eq!(CallConfig::from_json("nope"), CallConfig::default());
        assert!(custom_flag(Some(r#"{"network_standalone_reflectors":true}"#), "network_standalone_reflectors"));
        assert!(!custom_flag(Some("{}"), "network_standalone_reflectors"));
        assert!(!custom_flag(None, "x"));
    }
}
