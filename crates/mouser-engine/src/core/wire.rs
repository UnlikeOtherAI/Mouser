use mouser_core::ownership::OwnershipUpdate;
use mouser_core::platform::ScrollUnit as CoreScrollUnit;
use mouser_core::DeviceId;
use mouser_protocol::{
    from_cbor, Datagram, Goodbye, KeyEvent, OwnershipAck, OwnershipTransfer, Ping, PointerButton,
    PointerMotion, Pong, Scroll, ScrollUnit, TransferReason, TYPE_GOODBYE, TYPE_HEARTBEAT,
    TYPE_KEY_EVENT, TYPE_OWNERSHIP_ACK, TYPE_OWNERSHIP_TRANSFER, TYPE_PING, TYPE_POINTER_BUTTON,
    TYPE_PONG, TYPE_SCROLL,
};

use super::types::{InputRate, KeyTransition};
use super::{encode, Action, EngineCore, Inject, ReplayGuard, Role};

impl EngineCore {
    /// Process a decoded control frame from the peer.
    pub fn on_control(&mut self, msg_type: u16, payload: &[u8]) -> Vec<Action> {
        match msg_type {
            TYPE_OWNERSHIP_TRANSFER => self.on_transfer(payload),
            TYPE_OWNERSHIP_ACK => self.on_ownership_ack(payload),
            TYPE_KEY_EVENT => self.on_key(payload),
            TYPE_POINTER_BUTTON => self.on_button(payload),
            TYPE_SCROLL => self.on_scroll(payload),
            TYPE_PING => self.on_ping(payload),
            TYPE_PONG | TYPE_HEARTBEAT => {
                self.misses = 0;
                Vec::new()
            }
            TYPE_GOODBYE => self.on_goodbye(payload),
            _ => Vec::new(),
        }
    }

    fn on_transfer(&mut self, payload: &[u8]) -> Vec<Action> {
        let Ok(t) = from_cbor::<OwnershipTransfer>(payload) else {
            return Vec::new();
        };
        let Some(owner) = to_id(&t.to) else {
            return Vec::new();
        };
        if owner == self.me() && !self.input_auth.can_accept_grant() {
            let ack = encode(&OwnershipAck {
                owner_epoch: t.owner_epoch,
                accepted: false,
                reason: Some("input not permitted".to_string()),
            });
            return vec![Action::SendControl(TYPE_OWNERSHIP_ACK, ack)];
        }
        match self
            .ownership
            .observe_with_reason(owner, t.owner_epoch, t.reason)
        {
            OwnershipUpdate::Accepted { owner, epoch } => {
                self.guard = ReplayGuard::default();
                self.held_keys.clear();
                self.reset_out();
                self.misses = 0;
                self.pending_ack = None;
                if owner == self.me() {
                    self.input_auth.authorize_epoch(epoch);
                    self.input_rate = InputRate::full();
                    if self.role == Role::Source {
                        self.cross_out_armed = false;
                    }
                } else {
                    self.input_auth.revoke_epoch();
                    if self.role == Role::Source {
                        self.reclaim_armed = false;
                    }
                }
                let ack = encode(&OwnershipAck {
                    owner_epoch: epoch,
                    accepted: true,
                    reason: None,
                });
                let mut actions = vec![
                    Action::SendControl(TYPE_OWNERSHIP_ACK, ack),
                    Action::SetCursorVisible(owner == self.me()),
                    Action::OwnerChanged { owner, epoch },
                ];
                if self.role == Role::Source {
                    actions.push(Action::SetCaptureMode(self.capture_mode()));
                }
                actions
            }
            OwnershipUpdate::Rejected(_) => {
                let ack = encode(&OwnershipAck {
                    owner_epoch: t.owner_epoch,
                    accepted: false,
                    reason: Some("stale epoch".to_string()),
                });
                vec![Action::SendControl(TYPE_OWNERSHIP_ACK, ack)]
            }
        }
    }

    fn on_ownership_ack(&mut self, payload: &[u8]) -> Vec<Action> {
        let Ok(ack) = from_cbor::<OwnershipAck>(payload) else {
            return Vec::new();
        };
        let Some(pending) = self.pending_ack else {
            return Vec::new();
        };
        if ack.owner_epoch != pending.epoch {
            return Vec::new();
        }
        self.pending_ack = None;
        if !ack.accepted {
            return self.reclaim_local();
        }
        let seq = self.next_seq();
        vec![Action::SendMotion(PointerMotion {
            owner_epoch: ack.owner_epoch,
            seq,
            display_id: 0,
            x: self.peer_x,
            y: self.peer_y,
        })]
    }

    fn on_key(&mut self, payload: &[u8]) -> Vec<Action> {
        let Ok(k) = from_cbor::<KeyEvent>(payload) else {
            return Vec::new();
        };
        if !self.guard.accept_ctr(k.owner_epoch, k.ctr, self.epoch())
            || !self.authorize_inject(k.owner_epoch)
        {
            return Vec::new();
        }
        let transition = self.held_keys.observe(k.usage, k.down);
        if transition == KeyTransition::Repeat && !self.input_rate.allow_one() {
            return Vec::new();
        }
        if transition != KeyTransition::Repeat {
            let _ = self.input_rate.allow_one();
        }
        vec![Action::Inject(Inject::Key {
            usage: k.usage,
            down: k.down,
            mods: k.mods,
        })]
    }

    fn on_button(&mut self, payload: &[u8]) -> Vec<Action> {
        let Ok(b) = from_cbor::<PointerButton>(payload) else {
            return Vec::new();
        };
        if !self.guard.accept_ctr(b.owner_epoch, b.ctr, self.epoch())
            || !self.authorize_inject(b.owner_epoch)
        {
            return Vec::new();
        }
        let _ = self.input_rate.allow_one();
        vec![Action::Inject(Inject::Button {
            button: b.button,
            down: b.down,
        })]
    }

    fn on_scroll(&mut self, payload: &[u8]) -> Vec<Action> {
        let Ok(s) = from_cbor::<Scroll>(payload) else {
            return Vec::new();
        };
        if !self.guard.accept_ctr(s.owner_epoch, s.ctr, self.epoch())
            || !self.authorize_inject(s.owner_epoch)
            || !self.input_rate.allow_one()
        {
            return Vec::new();
        }
        let unit = match s.unit {
            ScrollUnit::Detent120 => CoreScrollUnit::Detent120,
            _ => CoreScrollUnit::LogicalPixel,
        };
        vec![Action::Inject(Inject::Scroll {
            dx: s.dx,
            dy: s.dy,
            unit,
        })]
    }

    fn on_ping(&mut self, payload: &[u8]) -> Vec<Action> {
        self.misses = 0;
        let Ok(p) = from_cbor::<Ping>(payload) else {
            return Vec::new();
        };
        vec![Action::SendControl(TYPE_PONG, encode(&Pong { id: p.id }))]
    }

    fn on_goodbye(&mut self, payload: &[u8]) -> Vec<Action> {
        let _ = from_cbor::<Goodbye>(payload);
        if self.role == Role::Source && !self.is_owner() {
            return self.reclaim_local();
        }
        Vec::new()
    }

    /// Process a pointer-motion datagram from the peer.
    pub fn on_motion(&mut self, datagram: Datagram) -> Vec<Action> {
        let Datagram::Motion(m) = datagram else {
            return Vec::new();
        };
        if !self.guard.accept_seq(m.owner_epoch, m.seq, self.epoch())
            || !self.authorize_inject(m.owner_epoch)
        {
            return Vec::new();
        }
        vec![Action::Inject(Inject::MoveCursor {
            display_id: m.display_id,
            x: m.x,
            y: m.y,
        })]
    }

    /// Runtime callback: a platform injection failed after the core had authorized it.
    pub fn on_injection_failed(&mut self) -> Vec<Action> {
        self.input_auth.set_input_allowed(false);
        self.input_auth.revoke_epoch();
        self.held_keys.clear();
        self.ownership.mark_input_blocked();
        if !self.is_owner() {
            return Vec::new();
        }
        let Some(epoch) = self.ownership.grant_to(self.peer) else {
            return Vec::new();
        };
        self.reset_out();
        self.guard = ReplayGuard::default();
        self.held_keys.clear();
        let transfer = encode(&OwnershipTransfer {
            to: self.peer.to_vec(),
            owner_epoch: epoch,
            layout_rev: 0,
            reason: TransferReason::UiSelect,
        });
        vec![
            Action::SendControl(TYPE_OWNERSHIP_TRANSFER, transfer),
            Action::SetCursorVisible(false),
            Action::OwnerChanged {
                owner: self.peer,
                epoch,
            },
        ]
    }

    pub(super) fn authorize_inject(&self, owner_epoch: u64) -> bool {
        self.is_owner() && self.input_auth.allows_epoch(owner_epoch)
    }
}

fn to_id(bytes: &[u8]) -> Option<DeviceId> {
    <[u8; 32]>::try_from(bytes).ok()
}
