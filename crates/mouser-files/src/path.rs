//! Path safety for received files (§7.8: "`name` sanitized on receipt — strip
//! separators, reject `..`, no symlink follow; files land in a quarantine dir").
//!
//! This is the security boundary that stops a malicious peer from writing outside the
//! quarantine directory (audit Round 1 **M3**). The attacker controls the offered
//! `name` string entirely, so we treat it as hostile:
//!
//! 1. Reject an **empty** name, or one that is `.`/`..`.
//! 2. Reject any name containing a path **separator** (`/` on every platform, plus `\`
//!    and a drive-letter `:` so a Windows-style `..\..\x` or `C:\x` can't slip through a
//!    Unix build) — the offered name must be a single path component.
//! 3. Reject interior NUL bytes (path truncation attacks).
//! 4. Join the sanitized component onto the quarantine dir and assert, via
//!    [`std::path::Component`] inspection (no filesystem access — pure), that the result
//!    is `quarantine / <one normal component>` and nothing escapes upward.
//!
//! Symlink safety is enforced at *write* time by the sink ([`crate::sink`]) opening
//! with `create_new` (never following an existing link) — documented there — because
//! "does this path traverse a symlink" cannot be answered purely without touching disk.

use std::path::{Component, Path, PathBuf};

/// A rejected file name (path-traversal / unsafe component). The offered transfer is
/// aborted rather than guessing a "safe" name, so the wire `name` can never write out
/// of quarantine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// The name was empty, `.`, or `..`.
    EmptyOrDotted,
    /// The name contained a path separator or drive letter (`/`, `\`, or `:`).
    ContainsSeparator,
    /// The name contained an interior NUL byte.
    ContainsNul,
    /// After joining, the path was not exactly `quarantine / <one normal component>`
    /// (defence-in-depth against an unforeseen `..`/root/prefix slipping through).
    EscapesQuarantine,
}

impl core::fmt::Display for PathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyOrDotted => f.write_str("file name is empty, '.', or '..'"),
            Self::ContainsSeparator => f.write_str("file name contains a path separator"),
            Self::ContainsNul => f.write_str("file name contains a NUL byte"),
            Self::EscapesQuarantine => f.write_str("resolved path escapes the quarantine dir"),
        }
    }
}

impl std::error::Error for PathError {}

/// Validate an offered `name` as a **single safe path component** (no resolution
/// against any directory). Returns the borrowed name on success.
///
/// Rejects: empty/`.`/`..`; anything with `/`, `\`, or `:`; interior NUL.
pub fn sanitize_name(name: &str) -> Result<&str, PathError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(PathError::EmptyOrDotted);
    }
    if name.contains('\0') {
        return Err(PathError::ContainsNul);
    }
    // A safe received name is a single component: it must contain no separators. We
    // reject `\` and `:` too so a Windows-style traversal can't pass on a Unix host
    // (where those aren't OS separators) and reach a Windows receiver via resume.
    if name.contains('/') || name.contains('\\') || name.contains(':') {
        return Err(PathError::ContainsSeparator);
    }
    Ok(name)
}

/// Resolve an offered `name` to the absolute path it MUST be written to, inside
/// `quarantine`. Pure (no disk access). The returned path is guaranteed to be
/// `quarantine` joined with exactly one [`Component::Normal`] equal to the sanitized
/// name — a `..`, root, or drive-prefix anywhere is rejected as
/// [`PathError::EscapesQuarantine`].
pub fn resolve_in_quarantine(quarantine: &Path, name: &str) -> Result<PathBuf, PathError> {
    let safe = sanitize_name(name)?;
    let candidate = quarantine.join(safe);

    // Defence in depth: strip the quarantine prefix and confirm what remains is a
    // single normal component. `std::path` parses `..`/prefixes/roots structurally,
    // so this catches anything `sanitize_name`'s string checks somehow missed.
    let rest = candidate
        .strip_prefix(quarantine)
        .map_err(|_| PathError::EscapesQuarantine)?;
    let mut comps = rest.components();
    let first = comps.next().ok_or(PathError::EscapesQuarantine)?;
    if comps.next().is_some() {
        return Err(PathError::EscapesQuarantine);
    }
    match first {
        Component::Normal(c) if c == std::ffi::OsStr::new(safe) => Ok(candidate),
        _ => Err(PathError::EscapesQuarantine),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_are_accepted() {
        assert_eq!(sanitize_name("report.pdf"), Ok("report.pdf"));
        assert_eq!(sanitize_name("a file with spaces.txt"), Ok("a file with spaces.txt"));
        assert_eq!(sanitize_name(".hidden"), Ok(".hidden")); // leading dot is fine
    }

    #[test]
    fn dotted_and_empty_rejected() {
        assert_eq!(sanitize_name(""), Err(PathError::EmptyOrDotted));
        assert_eq!(sanitize_name("."), Err(PathError::EmptyOrDotted));
        assert_eq!(sanitize_name(".."), Err(PathError::EmptyOrDotted));
    }

    #[test]
    fn separators_and_drive_letters_rejected() {
        assert_eq!(sanitize_name("a/b"), Err(PathError::ContainsSeparator));
        assert_eq!(sanitize_name("../etc/passwd"), Err(PathError::ContainsSeparator));
        assert_eq!(sanitize_name("..\\windows"), Err(PathError::ContainsSeparator));
        assert_eq!(sanitize_name("C:evil"), Err(PathError::ContainsSeparator));
    }

    #[test]
    fn nul_byte_rejected() {
        assert_eq!(sanitize_name("a\0b"), Err(PathError::ContainsNul));
    }

    #[test]
    fn traversal_resolves_to_rejection() {
        let q = Path::new("/var/mouser/quarantine");
        // The headline attack from the brief.
        assert_eq!(
            resolve_in_quarantine(q, "../../.ssh/authorized_keys"),
            Err(PathError::ContainsSeparator)
        );
        // Absolute path attempt.
        assert_eq!(
            resolve_in_quarantine(q, "/etc/passwd"),
            Err(PathError::ContainsSeparator)
        );
        // A clean name lands directly inside quarantine, nowhere else.
        assert_eq!(
            resolve_in_quarantine(q, "ok.bin"),
            Ok(PathBuf::from("/var/mouser/quarantine/ok.bin"))
        );
    }
}
