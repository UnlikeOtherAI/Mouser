//! On-disk daemon identity and trusted-peer store.
//!
//! The crypto identity lives in `mouser-core`; this module owns only the daemon I/O:
//! load the permanent local seed, create it on first launch, and keep the user-approved
//! peer pins that decide which devices may enter runtime paths.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use data_encoding::BASE32_NOPAD;
use mouser_core::{DeviceId, DeviceIdentity};

use crate::discovery;

const IDENTITY_SEED_FILE: &str = "identity.seed";
const TRUSTED_PEERS_FILE: &str = "trusted-peers.txt";
const SETTINGS_FILE: &str = "settings.json";

/// Errors loading or saving the daemon's persistent identity and trust pins.
#[derive(Debug, thiserror::Error)]
pub enum DaemonStoreError {
    /// The host did not expose a usable per-user data directory.
    #[error("could not find a per-user Mouser data directory")]
    MissingDataDir,
    /// Filesystem I/O failed for the named store path.
    #[error("{op} {path:?}: {source}")]
    Io {
        /// Operation being attempted.
        op: &'static str,
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Source I/O error.
        #[source]
        source: io::Error,
    },
    /// The identity seed file was neither raw 32-byte seed nor base32 text.
    #[error("identity seed at {path:?} is not a valid 32-byte Ed25519 seed")]
    InvalidIdentitySeed {
        /// Path to the invalid seed.
        path: PathBuf,
    },
    /// A trusted peer id was not valid Mouser base32 device id text.
    #[error("invalid trusted peer id at {path:?}:{line}: {value}")]
    InvalidTrustedPeer {
        /// Path to the invalid trust file.
        path: PathBuf,
        /// One-based line number.
        line: usize,
        /// Invalid line content.
        value: String,
    },
    /// A CLI-provided peer id was not valid Mouser base32 device id text.
    #[error("invalid peer id: {0}")]
    InvalidPeerId(String),
}

/// Per-user daemon store rooted in the OS app-data directory.
#[derive(Clone, Debug)]
pub struct DaemonStore {
    dir: PathBuf,
}

impl DaemonStore {
    /// Construct a store in the default OS-specific per-user Mouser directory.
    pub fn open_default() -> Result<Self, DaemonStoreError> {
        Ok(Self {
            dir: default_store_dir()?,
        })
    }

    /// Construct a store at an explicit directory (used by tests and embedders).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Load the daemon identity, creating and saving one on first launch.
    pub fn load_or_create_identity(&self) -> Result<DeviceIdentity, DaemonStoreError> {
        self.ensure_dir()?;
        let path = self.identity_seed_path();
        match fs::read(&path) {
            Ok(bytes) => {
                decode_identity_seed(&path, &bytes).map(|seed| DeviceIdentity::from_seed(&seed))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.create_identity(&path),
            Err(source) => Err(DaemonStoreError::Io {
                op: "read",
                path,
                source,
            }),
        }
    }

    /// Add a trusted peer by display/base32 id and return its raw `DeviceId`.
    pub fn trust_peer_base32(&self, peer_id: &str) -> Result<DeviceId, DaemonStoreError> {
        let id = parse_peer_id_arg(peer_id)?;
        self.trust_peer(id)?;
        Ok(id)
    }

    /// Add a trusted peer by raw `DeviceId`.
    pub fn trust_peer(&self, peer_id: DeviceId) -> Result<(), DaemonStoreError> {
        self.ensure_dir()?;
        let mut peers = self.load_trusted_peers()?;
        peers.insert(peer_id);
        self.write_trusted_peers(&peers)
    }

    /// Check whether a peer has been user-approved on this machine.
    pub fn is_peer_trusted(&self, peer_id: &DeviceId) -> Result<bool, DaemonStoreError> {
        Ok(self.load_trusted_peers()?.contains(peer_id))
    }

    /// Return all trusted peers, sorted by their raw id.
    pub fn trusted_peer_ids(&self) -> Result<Vec<DeviceId>, DaemonStoreError> {
        Ok(self.load_trusted_peers()?.into_iter().collect())
    }

    /// Load the persisted daemon settings, falling back to defaults when the file is
    /// absent or unreadable/corrupt (settings are best-effort, never fatal).
    pub fn load_settings(&self) -> mouser_ipc::SettingsDto {
        match fs::read(self.settings_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => mouser_ipc::SettingsDto::default(),
        }
    }

    /// Persist the daemon settings as pretty JSON (human-editable on disk too).
    pub fn save_settings(
        &self,
        settings: &mouser_ipc::SettingsDto,
    ) -> Result<(), DaemonStoreError> {
        self.ensure_dir()?;
        let path = self.settings_path();
        let body = serde_json::to_vec_pretty(settings).map_err(|e| DaemonStoreError::Io {
            op: "serialize settings",
            path: path.clone(),
            source: io::Error::other(e),
        })?;
        fs::write(&path, body).map_err(|source| DaemonStoreError::Io {
            op: "write",
            path,
            source,
        })
    }

    fn settings_path(&self) -> PathBuf {
        self.dir.join(SETTINGS_FILE)
    }

    fn identity_seed_path(&self) -> PathBuf {
        self.dir.join(IDENTITY_SEED_FILE)
    }

    fn trusted_peers_path(&self) -> PathBuf {
        self.dir.join(TRUSTED_PEERS_FILE)
    }

    fn ensure_dir(&self) -> Result<(), DaemonStoreError> {
        fs::create_dir_all(&self.dir).map_err(|source| DaemonStoreError::Io {
            op: "create directory",
            path: self.dir.clone(),
            source,
        })
    }

    fn create_identity(&self, path: &Path) -> Result<DeviceIdentity, DaemonStoreError> {
        let identity = DeviceIdentity::generate();
        match write_new_identity_seed(path, &identity) {
            Ok(()) => Ok(identity),
            Err(DaemonStoreError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                let bytes = fs::read(path).map_err(|source| DaemonStoreError::Io {
                    op: "read",
                    path: path.to_path_buf(),
                    source,
                })?;
                decode_identity_seed(path, &bytes).map(|seed| DeviceIdentity::from_seed(&seed))
            }
            Err(e) => Err(e),
        }
    }

    fn load_trusted_peers(&self) -> Result<BTreeSet<DeviceId>, DaemonStoreError> {
        let path = self.trusted_peers_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
            Err(source) => {
                return Err(DaemonStoreError::Io {
                    op: "read",
                    path,
                    source,
                });
            }
        };

        let mut peers = BTreeSet::new();
        for (line_index, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let id = discovery::decode_device_id(trimmed).ok_or_else(|| {
                DaemonStoreError::InvalidTrustedPeer {
                    path: path.clone(),
                    line: line_index.saturating_add(1),
                    value: trimmed.to_string(),
                }
            })?;
            peers.insert(id);
        }
        Ok(peers)
    }

    fn write_trusted_peers(&self, peers: &BTreeSet<DeviceId>) -> Result<(), DaemonStoreError> {
        let path = self.trusted_peers_path();
        let mut body = String::new();
        for peer in peers {
            body.push_str(&format_device_id(peer));
            body.push('\n');
        }
        fs::write(&path, body).map_err(|source| DaemonStoreError::Io {
            op: "write",
            path,
            source,
        })
    }
}

/// Parse a display/base32 peer id supplied on the CLI.
pub fn parse_peer_id_arg(peer_id: &str) -> Result<DeviceId, DaemonStoreError> {
    discovery::decode_device_id(peer_id)
        .ok_or_else(|| DaemonStoreError::InvalidPeerId(peer_id.trim().to_string()))
}

/// Display-only base32 text for a raw device id.
pub fn format_device_id(device_id: &DeviceId) -> String {
    BASE32_NOPAD.encode(device_id).to_lowercase()
}

fn decode_identity_seed(path: &Path, bytes: &[u8]) -> Result<[u8; 32], DaemonStoreError> {
    if bytes.len() == 32 {
        return <[u8; 32]>::try_from(bytes).map_err(|_| DaemonStoreError::InvalidIdentitySeed {
            path: path.to_path_buf(),
        });
    }

    let text = std::str::from_utf8(bytes).map_err(|_| DaemonStoreError::InvalidIdentitySeed {
        path: path.to_path_buf(),
    })?;
    let decoded = BASE32_NOPAD
        .decode(text.trim().to_uppercase().as_bytes())
        .map_err(|_| DaemonStoreError::InvalidIdentitySeed {
            path: path.to_path_buf(),
        })?;
    <[u8; 32]>::try_from(decoded.as_slice()).map_err(|_| DaemonStoreError::InvalidIdentitySeed {
        path: path.to_path_buf(),
    })
}

fn write_new_identity_seed(path: &Path, identity: &DeviceIdentity) -> Result<(), DaemonStoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_create_mode(&mut options);
    let mut file = options.open(path).map_err(|source| DaemonStoreError::Io {
        op: "create",
        path: path.to_path_buf(),
        source,
    })?;
    let seed = identity.secret_seed();
    let mut encoded = BASE32_NOPAD.encode(&seed).to_lowercase();
    encoded.push('\n');
    file.write_all(encoded.as_bytes())
        .map_err(|source| DaemonStoreError::Io {
            op: "write",
            path: path.to_path_buf(),
            source,
        })?;
    set_private_permissions(path)
}

#[cfg(unix)]
fn set_private_create_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_create_mode(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), DaemonStoreError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        DaemonStoreError::Io {
            op: "chmod",
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), DaemonStoreError> {
    Ok(())
}

fn default_store_dir() -> Result<PathBuf, DaemonStoreError> {
    #[cfg(target_os = "windows")]
    {
        if let Some(base) = env_path("LOCALAPPDATA").or_else(|| env_path("APPDATA")) {
            return Ok(base.join("Mouser"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            return Ok(home
                .join("Library")
                .join("Application Support")
                .join("Mouser"));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(base) = env_path("XDG_DATA_HOME") {
            return Ok(base.join("mouser"));
        }
        if let Some(home) = home_dir() {
            return Ok(home.join(".local").join("share").join("mouser"));
        }
    }

    home_dir()
        .map(|home| home.join(".mouser"))
        .ok_or(DaemonStoreError::MissingDataDir)
}

fn home_dir() -> Option<PathBuf> {
    env_path("USERPROFILE").or_else(|| env_path("HOME"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name)?;
    if value.to_string_lossy().is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_persists_between_loads() {
        let dir = unique_test_dir("identity");
        let store = DaemonStore::new(&dir);

        let first = store.load_or_create_identity().expect("create identity");
        let second = store.load_or_create_identity().expect("load identity");

        assert_eq!(first.device_id(), second.device_id());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn identity_seed_is_private_on_create() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_test_dir("identity-private");
        let store = DaemonStore::new(&dir);
        let _identity = store.load_or_create_identity().expect("create identity");

        let mode = fs::metadata(dir.join(IDENTITY_SEED_FILE))
            .expect("identity metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn trusted_peers_persist_and_dedupe() {
        let dir = unique_test_dir("trusted");
        let store = DaemonStore::new(&dir);
        let peer = DeviceIdentity::generate();

        store.trust_peer(peer.device_id()).expect("trust peer");
        store.trust_peer(peer.device_id()).expect("trust duplicate");

        assert!(store
            .is_peer_trusted(&peer.device_id())
            .expect("trusted lookup"));
        assert_eq!(
            store.trusted_peer_ids().expect("trusted peers"),
            vec![peer.device_id()]
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn trusted_peer_base32_round_trips() {
        let dir = unique_test_dir("base32");
        let store = DaemonStore::new(&dir);
        let peer = DeviceIdentity::generate();

        let parsed = store
            .trust_peer_base32(&peer.device_id_base32())
            .expect("trust base32");

        assert_eq!(parsed, peer.device_id());
        assert!(store
            .is_peer_trusted(&peer.device_id())
            .expect("trusted lookup"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_trusted_peer_line_errors() {
        let dir = unique_test_dir("invalid");
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(dir.join(TRUSTED_PEERS_FILE), "nope\n").expect("write trust file");
        let store = DaemonStore::new(&dir);

        assert!(matches!(
            store.trusted_peer_ids(),
            Err(DaemonStoreError::InvalidTrustedPeer { line: 1, .. })
        ));
        let _ = fs::remove_dir_all(dir);
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        dir.push(format!(
            "mouser-engine-{name}-{}-{nanos}",
            std::process::id()
        ));
        dir
    }
}
