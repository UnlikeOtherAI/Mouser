//! The well-known local IPC endpoint path.
//!
//! Both ends agree on this path so the UI can find the daemon without configuration.
//! On Unix this is a Unix-domain socket in the per-user runtime directory. On Windows
//! it is a local named pipe under `\\.\pipe`.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

#[cfg(unix)]
use std::fs::{self, DirBuilder, Permissions};
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

#[cfg(unix)]
use nix::unistd::Uid;

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

#[cfg(unix)]
pub(crate) fn prepare_default_socket_parent(path: &Path) -> io::Result<()> {
    if xdg_runtime_dir().is_some() || path != default_socket_path() {
        return Ok(());
    }
    ensure_private_dir(&fallback_runtime_dir())
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

#[cfg(unix)]
fn runtime_dir() -> PathBuf {
    xdg_runtime_dir().unwrap_or_else(fallback_runtime_dir)
}

#[cfg(unix)]
fn xdg_runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn fallback_runtime_dir() -> PathBuf {
    std::env::temp_dir().join(format!("mouser-{}", Uid::current().as_raw()))
}

#[cfg(unix)]
fn ensure_private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            match builder.create(path) {
                Ok(()) => {}
                Err(create_err) if create_err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(create_err) => return Err(create_err),
            }
        }
        Err(e) => return Err(e),
    }

    let metadata = fs::symlink_metadata(path)?;
    validate_private_dir_kind_and_owner(path, &metadata)?;
    if metadata.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, Permissions::from_mode(0o700))?;
    }
    validate_private_dir(path, &fs::symlink_metadata(path)?)
}

#[cfg(unix)]
fn validate_private_dir_kind_and_owner(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "ipc runtime path is not a private directory: {}",
                path.display()
            ),
        ));
    }
    if metadata.uid() != Uid::current().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("ipc runtime directory owner mismatch: {}", path.display()),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_dir(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    validate_private_dir_kind_and_owner(path, metadata)?;
    if metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("ipc runtime directory is not 0700: {}", path.display()),
        ));
    }
    Ok(())
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
