use automerge::{AutoCommit, Change, ChangeHash};

use crate::error::{StateError, StateResult};
use crate::state::{genesis_doc, SharedState};
use crate::wire;

impl SharedState {
    /// Full save for `StateSnapshot.full_state` (spec §7.2 `[13]`).
    ///
    /// Wire snapshots are emitted without automerge deflated columns so peers
    /// can enforce §0.3 bounds before decode. Callers that retain snapshots for
    /// long-lived storage should compact before [`wire::SNAPSHOT_WIRE_CAP`].
    pub fn snapshot(&self) -> Vec<u8> {
        let mut doc = self.doc.clone();
        doc.save_nocompress()
    }

    /// Full save for network transmission, rejecting history that has grown
    /// beyond the §0.3 snapshot cap instead of emitting an oversized payload.
    pub fn snapshot_for_wire(&self) -> StateResult<Vec<u8>> {
        let bytes = self.snapshot();
        wire::validate_snapshot_bytes(&bytes)?;
        Ok(bytes)
    }

    /// Number of automerge changes retained in this document. This is the
    /// compaction hook for callers watching unbounded history/tombstone growth.
    #[must_use]
    pub fn history_change_count(&self) -> usize {
        let mut doc = self.doc.clone();
        doc.get_changes(&[]).len()
    }

    /// Load a full document from a `StateSnapshot.full_state` payload.
    pub fn load(bytes: &[u8]) -> StateResult<Self> {
        wire::validate_snapshot_bytes(bytes)?;
        let doc = AutoCommit::load(bytes).map_err(StateError::Automerge)?;
        ensure_shared_genesis(&doc)?;
        Ok(SharedState { doc })
    }

    /// Current document heads (spec `StateRequest.have_heads` / `StateDelta.dep_heads`).
    #[must_use]
    pub fn heads(&self) -> Vec<ChangeHash> {
        let mut doc = self.doc.clone();
        doc.get_heads()
    }

    /// All changes this document holds that are **not** transitive dependencies
    /// of `have_heads` — the reply payload for a `StateRequest` (`StateChanges.changes`).
    /// Each entry is one encoded automerge change (`StateDelta.change` bytes).
    #[must_use]
    pub fn changes_since(&self, have_heads: &[ChangeHash]) -> Vec<Vec<u8>> {
        let mut doc = self.doc.clone();
        doc.get_changes(have_heads)
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect()
    }

    /// Apply encoded changes received via `StateDelta`/`StateChanges`.
    ///
    /// Each change is decoded and applied independently. Malformed entries and
    /// forged duplicate actor/seq changes are skipped so one bad item cannot
    /// wedge anti-entropy for the rest of the batch.
    pub fn apply_changes(&mut self, changes: &[Vec<u8>]) -> StateResult<()> {
        let mut saw_valid_change = false;
        let mut first_decode_error = None;

        for bytes in changes {
            let decoded = wire::validate_change_bytes(bytes).and_then(|()| {
                Change::from_bytes(bytes.clone()).map_err(|e| StateError::Decode(e.to_string()))
            });
            match decoded {
                Ok(change) => {
                    saw_valid_change = true;
                    if let Err(err) = self.doc.apply_changes(std::iter::once(change)) {
                        if !matches!(err, automerge::AutomergeError::DuplicateSeqNumber(_, _)) {
                            return Err(StateError::Automerge(err));
                        }
                    }
                }
                Err(err) => {
                    if first_decode_error.is_none() {
                        first_decode_error = Some(err);
                    }
                }
            }
        }

        if saw_valid_change {
            Ok(())
        } else if let Some(err) = first_decode_error {
            Err(err)
        } else {
            Ok(())
        }
    }

    /// Merge another document into this one (used for snapshot reconciliation).
    pub fn merge(&mut self, other: &SharedState) -> StateResult<()> {
        let mut other = other.doc.clone();
        self.doc.merge(&mut other)?;
        Ok(())
    }

    /// Change hashes still missing for the given heads (callers may request them).
    #[must_use]
    pub fn missing_deps(&self, heads: &[ChangeHash]) -> Vec<ChangeHash> {
        let mut doc = self.doc.clone();
        doc.get_missing_deps(heads)
    }
}

fn ensure_shared_genesis(doc: &AutoCommit) -> StateResult<()> {
    let mut genesis = genesis_doc();
    let genesis_heads = genesis.get_heads();
    let mut candidate = doc.clone();
    let has_genesis = candidate
        .get_changes(&[])
        .iter()
        .any(|change| genesis_heads.iter().any(|head| *head == change.hash()));
    if has_genesis {
        Ok(())
    } else {
        Err(StateError::Schema(
            "snapshot is not derived from the shared genesis".to_owned(),
        ))
    }
}
