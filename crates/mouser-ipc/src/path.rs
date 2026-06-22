//! The well-known Unix-domain socket path for the local IPC link.
//!
//! Both ends agree on this path so the UI can find the daemon without configuration.
//! It lives in the per-user runtime directory (`$XDG_RUNTIME_DIR` on Linux when set,
//! otherwise the system temp dir), so it is private to the logged-in user and cleaned
//! up across reboots.

use std::path::PathBuf;

/// File name of the daemon's IPC socket within the runtime directory.
pub const SOCKET_FILE: &str = "mouserd.sock";

/// The well-known path of the daemon's IPC socket for the current user.
///
/// Resolution order: `$XDG_RUNTIME_DIR` (Linux session runtime dir, when set) then the
/// OS temp directory ([`std::env::temp_dir`], which honors `$TMPDIR` on macOS).
pub fn default_socket_path() -> PathBuf {
    runtime_dir().join(SOCKET_FILE)
}

fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    std::env::temp_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_ends_with_known_file() {
        let path = default_socket_path();
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(SOCKET_FILE)
        );
    }
}
