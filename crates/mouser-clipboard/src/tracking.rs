//! Bounded bookkeeping for clipboard pulls and loop-prevention markers.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash as StdHash, Hasher};

use mouser_core::DeviceId;
use mouser_protocol::{ClipFormat, Os};

use crate::reassembly::{Progress, Reassembly};
use crate::{ClipboardError, Hash};

/// Maximum in-flight inbound pulls retained by one engine instance.
pub const MAX_PENDING_PULLS: usize = 64;

/// Maximum recently-applied representations retained for loop prevention.
pub const MAX_APPLIED_CLIPS: usize = 128;

/// Logical ticks before an in-flight pull is considered stalled.
pub const PULL_STALL_TICKS: u64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PullKey {
    pub(crate) origin: DeviceId,
    pub(crate) format: ClipFormat,
    pub(crate) hash: Hash,
}

impl PullKey {
    pub(crate) fn new(origin: DeviceId, format: ClipFormat, hash: Hash) -> Self {
        Self {
            origin,
            format,
            hash,
        }
    }
}

impl StdHash for PullKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.origin.hash(state);
        u16::from(self.format).hash(state);
        self.hash.hash(state);
    }
}

pub(crate) struct Pending {
    key: PullKey,
    peer_os: Os,
    deadline_tick: u64,
    reasm: Reassembly,
}

impl Pending {
    pub(crate) fn new(key: PullKey, size: u64, peer_os: Os, now_tick: u64) -> Self {
        Self {
            key,
            peer_os,
            deadline_tick: now_tick.saturating_add(PULL_STALL_TICKS),
            reasm: Reassembly::new(key.format, key.hash, size),
        }
    }

    pub(crate) fn key(&self) -> PullKey {
        self.key
    }

    pub(crate) fn origin(&self) -> DeviceId {
        self.key.origin
    }

    pub(crate) fn format(&self) -> ClipFormat {
        self.key.format
    }

    pub(crate) fn peer_os(&self) -> Os {
        self.peer_os
    }

    pub(crate) fn progress(&self) -> Progress {
        self.reasm.progress()
    }

    pub(crate) fn push(
        &mut self,
        offset: u64,
        data: &[u8],
        last: bool,
    ) -> Result<Option<Vec<u8>>, ClipboardError> {
        self.reasm.push(offset, data, last)
    }

    fn is_stalled(&self, now_tick: u64) -> bool {
        now_tick >= self.deadline_tick
    }
}

#[derive(Default)]
pub(crate) struct PendingPulls {
    entries: HashMap<PullKey, Pending>,
    order: VecDeque<PullKey>,
}

impl PendingPulls {
    pub(crate) fn insert(&mut self, pending: Pending) {
        let key = pending.key();
        self.remove(&key);
        while self.entries.len() >= MAX_PENDING_PULLS {
            if !self.evict_oldest() {
                break;
            }
        }
        self.order.push_back(key);
        self.entries.insert(key, pending);
    }

    pub(crate) fn get_mut(&mut self, key: &PullKey) -> Option<&mut Pending> {
        self.entries.get_mut(key)
    }

    pub(crate) fn remove(&mut self, key: &PullKey) -> Option<Pending> {
        self.order.retain(|queued| queued != key);
        self.entries.remove(key)
    }

    pub(crate) fn remove_hash(&mut self, hash: &Hash) -> bool {
        let keys = self
            .entries
            .keys()
            .filter(|key| key.hash == *hash)
            .copied()
            .collect::<Vec<_>>();
        let removed = !keys.is_empty();
        for key in keys {
            self.remove(&key);
        }
        removed
    }

    pub(crate) fn remove_origin(&mut self, origin: DeviceId) -> usize {
        let keys = self
            .entries
            .keys()
            .filter(|key| key.origin == origin)
            .copied()
            .collect::<Vec<_>>();
        let removed = keys.len();
        for key in keys {
            self.remove(&key);
        }
        removed
    }

    pub(crate) fn retain_origin_offer(&mut self, origin: DeviceId, offered: &[PullKey]) {
        let stale = self
            .entries
            .keys()
            .filter(|key| key.origin == origin && !offered.contains(key))
            .copied()
            .collect::<Vec<_>>();
        for key in stale {
            self.remove(&key);
        }
    }

    pub(crate) fn expire_stalled(&mut self, now_tick: u64) -> usize {
        let keys = self
            .entries
            .iter()
            .filter(|(_, pending)| pending.is_stalled(now_tick))
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        let removed = keys.len();
        for key in keys {
            self.remove(&key);
        }
        removed
    }

    pub(crate) fn key_for_origin_data(
        &self,
        origin: DeviceId,
        format: ClipFormat,
        hash: Hash,
    ) -> Result<PullKey, ClipboardError> {
        let key = PullKey::new(origin, format, hash);
        if self.entries.contains_key(&key) {
            return Ok(key);
        }
        if self
            .entries
            .keys()
            .any(|candidate| candidate.origin == origin && candidate.hash == hash)
        {
            return Err(ClipboardError::Protocol("data format mismatch".into()));
        }
        Err(ClipboardError::UnknownHash)
    }

    pub(crate) fn key_for_data(
        &self,
        format: ClipFormat,
        hash: Hash,
    ) -> Result<PullKey, ClipboardError> {
        let mut matches = self
            .entries
            .keys()
            .filter(|key| key.format == format && key.hash == hash)
            .copied();
        let Some(first) = matches.next() else {
            if self.entries.keys().any(|key| key.hash == hash) {
                return Err(ClipboardError::Protocol("data format mismatch".into()));
            }
            return Err(ClipboardError::UnknownHash);
        };
        if matches.next().is_some() {
            return Err(ClipboardError::Protocol(
                "ambiguous clipboard data origin".into(),
            ));
        }
        Ok(first)
    }

    pub(crate) fn progress_by_hash(&self, hash: &Hash) -> Option<Progress> {
        self.entries
            .values()
            .find(|pending| pending.key.hash == *hash)
            .map(Pending::progress)
    }

    pub(crate) fn contains_hash(&self, hash: &Hash) -> bool {
        self.entries.keys().any(|key| key.hash == *hash)
    }

    fn evict_oldest(&mut self) -> bool {
        while let Some(key) = self.order.pop_front() {
            if self.entries.remove(&key).is_some() {
                return true;
            }
        }
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AppliedKey {
    origin: DeviceId,
    format: ClipFormat,
    hash: Hash,
}

impl AppliedKey {
    fn new(origin: DeviceId, format: ClipFormat, hash: Hash) -> Self {
        Self {
            origin,
            format,
            hash,
        }
    }
}

struct AppliedMarker {
    key: AppliedKey,
    suppress_next: Cell<bool>,
}

#[derive(Default)]
pub(crate) struct AppliedLog {
    entries: VecDeque<AppliedMarker>,
}

impl AppliedLog {
    pub(crate) fn record(&mut self, origin: DeviceId, format: ClipFormat, hash: Hash) {
        let key = AppliedKey::new(origin, format, hash);
        self.entries.retain(|entry| entry.key != key);
        while self.entries.len() >= MAX_APPLIED_CLIPS {
            if self.entries.pop_front().is_none() {
                break;
            }
        }
        self.entries.push_back(AppliedMarker {
            key,
            suppress_next: Cell::new(true),
        });
    }

    pub(crate) fn suppress_once(&self, format: ClipFormat, hash: &Hash) -> bool {
        let Some(marker) = self.entries.iter().find(|entry| {
            entry.suppress_next.get() && entry.key.format == format && entry.key.hash == *hash
        }) else {
            return false;
        };
        marker.suppress_next.set(false);
        true
    }

    pub(crate) fn contains_hash(&self, hash: &Hash) -> bool {
        self.entries.iter().any(|entry| entry.key.hash == *hash)
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}
