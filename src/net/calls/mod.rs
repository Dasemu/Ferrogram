//! One-to-one voice calls (tgcalls protocol V2, library versions 8.0.0 and
//! 9.0.0). See `docs/calls-feasibility.md` and `docs/protocol.md`.
//!
//! Nothing here runs until a call starts: the engine, its sockets and the
//! audio threads are created per call and dropped when it ends.

pub mod crypto;
pub mod dh;
pub mod emoji;
pub mod signaling;
pub mod stun;
