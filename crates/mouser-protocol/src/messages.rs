//! Control-plane message types and their envelope `type` discriminants (§7).
//! Only the messages needed for the first milestone are defined here; the rest land
//! as the engine grows, each as an additive `type` (§2 forward-compatibility).

use serde::{Deserialize, Serialize};

/// `[02] HelloAck` envelope type.
pub const TYPE_HELLO_ACK: u16 = 0x02;
/// `[05] Ping` envelope type.
pub const TYPE_PING: u16 = 0x05;

/// `[05] Ping { id: u64 }` — liveness probe; `Pong` echoes `id` for same-clock RTT (§7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ping {
    pub id: u64,
}
