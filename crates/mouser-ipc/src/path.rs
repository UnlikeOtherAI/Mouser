//! The well-known local IPC endpoint path.
//!
//! Both ends agree on this path so the UI can find the daemon without configuration.
//! On Unix this is a Unix-domain socket in the per-user runtime directory. On Windows
//! it is a local named pipe under `\\.\pipe`.

use std::path::PathBuf;

/// File name of the daemon's IPC socket/pipe within the platform endpoint namespace.
pub const SOCKET_FILE: &str = "mouserd.sock";

/// The well-known IPC endpoint for the current user.
pub fn default_socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(format!(r"\\.\pipe\{}-{}", SOCKET_FILE, user_suffix()))
    }

    #[cfg(not(windows))]
    {
        runtime_dir().join(SOCKET_FILE)
    }
}

/// Build a unique local IPC endpoint path for tests.
#[cfg(test)]
pub fn test_socket_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("mouser-ipc-{tag}-{}-{nanos}.sock", std::process::id());

    #[cfg(windows)]
    {
        PathBuf::from(format!(r"\\.\pipe\{name}"))
    }

    #[cfg(not(windows))]
    {
        std::env::temp_dir().join(name)
    }
}

#[cfg(not(windows))]
fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    std::env::temp_dir()
}

/// A conservative per-user suffix so multiple Windows users do not fight over one
/// global pipe name. Characters outside a simple filename set are collapsed.
#[cfg(windows)]
fn user_suffix() -> String {
    let raw = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".to_string());
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "user".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_uses_known_endpoint_name() {
        let path = default_socket_path();

        #[cfg(windows)]
        {
            let rendered = path.to_string_lossy();
            assert!(rendered.starts_with(r"\\.\pipe\"));
            assert!(rendered.contains(SOCKET_FILE));
        }

        #[cfg(not(windows))]
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(SOCKET_FILE));
    }

    #[test]
    fn test_socket_path_is_platform_local() {
        let path = test_socket_path("probe");

        #[cfg(windows)]
        assert!(path.to_string_lossy().starts_with(r"\\.\pipe\"));

        #[cfg(not(windows))]
        assert_eq!(
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains("probe")),
            Some(true)
        );
    }
}
