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
//! 4. Reject names that are unsafe specifically on a **Windows** receiver even though
//!    they look benign on Unix (audit R2): the reserved DOS device names (`CON`, `PRN`,
//!    `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, case-insensitive, with or without an
//!    extension), and a trailing dot or space (Windows silently strips those, so
//!    `foo.txt.` and `foo.txt ` collide with / overwrite `foo.txt`).
//! 5. Reject Unicode **bidirectional-control** codepoints (RLO/LRO/PDF/…): they let an
//!    attacker visually spoof an extension (`exe`-as-`txt`) in any UI that renders the
//!    name. These stay inside quarantine, so they are anti-spoof/anti-collision hardening
//!    rather than a traversal escape.
//! 6. Join the sanitized component onto the quarantine dir and assert, via
//!    [`std::path::Component`] inspection (no filesystem access — pure), that the result
//!    is `quarantine / <one normal component>` and nothing escapes upward.
//!
//! Symlink safety is enforced at *write* time by the disk sink
//! ([`crate::fs_sink::FsSink`]) `lstat`-ing the target and refusing a pre-existing
//! symlink — documented there — because "does this path traverse a symlink" cannot be
//! answered purely without touching disk.

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
    /// The name is a Windows reserved DOS device name (`CON`, `NUL`, `COM1`, …), which
    /// cannot be created as a file on a Windows receiver.
    ReservedDeviceName,
    /// The name ends in a dot or space, which Windows silently strips (a collision /
    /// overwrite vector against the un-suffixed name).
    TrailingDotOrSpace,
    /// The name contains a Unicode bidirectional-control codepoint (extension-spoofing).
    BidiControl,
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
            Self::ReservedDeviceName => f.write_str("file name is a reserved device name"),
            Self::TrailingDotOrSpace => f.write_str("file name ends in a dot or space"),
            Self::BidiControl => f.write_str("file name contains a bidi-control codepoint"),
            Self::EscapesQuarantine => f.write_str("resolved path escapes the quarantine dir"),
        }
    }
}

impl std::error::Error for PathError {}

/// Validate an offered `name` as a **single safe path component** (no resolution
/// against any directory). Returns the borrowed name on success.
///
/// Rejects: empty/`.`/`..`; anything with `/`, `\`, or `:`; interior NUL; a Windows
/// reserved device name; a trailing dot/space; and any bidi-control codepoint.
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
    // Bidi-control codepoints visually spoof the extension in any name-rendering UI.
    if name.chars().any(is_bidi_control) {
        return Err(PathError::BidiControl);
    }
    // Windows strips a trailing dot/space, collapsing `foo.txt.`/`foo.txt ` onto
    // `foo.txt` — a silent overwrite/collision. Reject so the name is stable everywhere.
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(PathError::TrailingDotOrSpace);
    }
    // Reserved DOS device names can't become files on Windows (with or without an ext).
    if is_reserved_device_name(name) {
        return Err(PathError::ReservedDeviceName);
    }
    Ok(name)
}

/// Whether `c` is a Unicode bidirectional-control codepoint (the override/embedding/
/// isolate set, U+202A–U+202E and U+2066–U+2069, plus the legacy U+200E/U+200F marks).
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'
            | '\u{202B}'
            | '\u{202C}'
            | '\u{202D}'
            | '\u{202E}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
    )
}

/// Whether `name` is a Windows reserved DOS device name (case-insensitive), considering
/// only the stem before the first `.` — `CON`, `con.txt`, and `COM1.tar.gz` all match.
fn is_reserved_device_name(name: &str) -> bool {
    // Windows resolves a device name by the stem before the first dot, so check that.
    let stem = name.split('.').next().unwrap_or(name);
    const FIXED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    if FIXED.iter().any(|d| stem.eq_ignore_ascii_case(d)) {
        return true;
    }
    // COM1–COM9 / LPT1–LPT9: a 3-letter prefix followed by a single digit 1..=9.
    let bytes = stem.as_bytes();
    if bytes.len() == 4 {
        let (prefix, last) = stem.split_at(3);
        let is_com_or_lpt =
            prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT");
        if is_com_or_lpt && matches!(last.as_bytes().first(), Some(b'1'..=b'9')) {
            return true;
        }
    }
    false
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
        assert_eq!(
            sanitize_name("a file with spaces.txt"),
            Ok("a file with spaces.txt")
        );
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
        assert_eq!(
            sanitize_name("../etc/passwd"),
            Err(PathError::ContainsSeparator)
        );
        assert_eq!(
            sanitize_name("..\\windows"),
            Err(PathError::ContainsSeparator)
        );
        assert_eq!(sanitize_name("C:evil"), Err(PathError::ContainsSeparator));
    }

    #[test]
    fn nul_byte_rejected() {
        assert_eq!(sanitize_name("a\0b"), Err(PathError::ContainsNul));
    }

    #[test]
    fn windows_reserved_device_names_rejected() {
        // Bare, case-insensitive.
        assert_eq!(sanitize_name("CON"), Err(PathError::ReservedDeviceName));
        assert_eq!(sanitize_name("nul"), Err(PathError::ReservedDeviceName));
        assert_eq!(sanitize_name("Aux"), Err(PathError::ReservedDeviceName));
        assert_eq!(sanitize_name("PRN"), Err(PathError::ReservedDeviceName));
        // With an extension — Windows still resolves the device by the stem.
        assert_eq!(sanitize_name("con.txt"), Err(PathError::ReservedDeviceName));
        assert_eq!(
            sanitize_name("COM1.tar.gz"),
            Err(PathError::ReservedDeviceName)
        );
        // COM1–COM9 / LPT1–LPT9.
        assert_eq!(sanitize_name("COM1"), Err(PathError::ReservedDeviceName));
        assert_eq!(sanitize_name("lpt9"), Err(PathError::ReservedDeviceName));
        // Adjacent non-reserved names are fine: COM0 / LPT0 / COM10 are NOT devices,
        // and a longer name that merely starts with the prefix is fine.
        assert_eq!(sanitize_name("COM0"), Ok("COM0"));
        assert_eq!(sanitize_name("LPT0"), Ok("LPT0"));
        assert_eq!(sanitize_name("COM10"), Ok("COM10"));
        assert_eq!(sanitize_name("CONSOLE"), Ok("CONSOLE"));
        assert_eq!(sanitize_name("console.log"), Ok("console.log"));
    }

    #[test]
    fn trailing_dot_or_space_rejected() {
        assert_eq!(
            sanitize_name("report.pdf."),
            Err(PathError::TrailingDotOrSpace)
        );
        assert_eq!(
            sanitize_name("report.pdf "),
            Err(PathError::TrailingDotOrSpace)
        );
        // A leading/interior dot or space is fine; only the trailing one is the trap.
        assert_eq!(sanitize_name(".hidden"), Ok(".hidden"));
        assert_eq!(sanitize_name("a b.txt"), Ok("a b.txt"));
    }

    #[test]
    fn bidi_control_codepoints_rejected() {
        // RLO mid-name (the classic `exe`→`txt` spoof).
        assert_eq!(
            sanitize_name("invoice\u{202E}cod.exe"),
            Err(PathError::BidiControl)
        );
        // A couple more from the override/isolate set.
        assert_eq!(sanitize_name("a\u{202D}b"), Err(PathError::BidiControl));
        assert_eq!(sanitize_name("a\u{2066}b"), Err(PathError::BidiControl));
        assert_eq!(sanitize_name("a\u{200F}b"), Err(PathError::BidiControl));
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
