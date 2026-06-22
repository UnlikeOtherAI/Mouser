import Foundation

/// Translates typed ASCII characters into the wire key events the engine expects:
/// **USB HID Usage Page 0x07** usage codes plus a `mods` bitmask (Appendix B).
///
/// The companion only has a software keyboard, so we synthesise HID usages from
/// the characters the field reports rather than from physical key events. The
/// table is the inverse of a US-QWERTY layout: each character maps to the usage
/// of the physical key that produces it, and `shift` records whether that key
/// needs Left-Shift held (e.g. `A`, `!`, `?`).
///
/// `mods` bit order is Appendix B: bit0 LCtrl, **bit1 LShift**, bit2 LAlt, … —
/// so the shift flag becomes `1 << 1`. Unknown characters return `nil` and are
/// skipped by the caller.
enum HidKeymap {
    /// Appendix B `mods` bitmask: Left-Shift is bit 1.
    static let shiftMod: UInt16 = 1 << 1

    /// A single resolved key: the HID usage and whether Left-Shift is required.
    struct Stroke {
        let usage: UInt16
        let shift: Bool

        /// The `mods` bitmask to send alongside `usage`.
        var mods: UInt16 { shift ? HidKeymap.shiftMod : 0 }
    }

    // Named usages reused below / by callers (Return on submit, Backspace on delete).
    static let returnUsage: UInt16 = 0x28
    static let backspaceUsage: UInt16 = 0x2A

    /// Resolve a character to its HID stroke, or `nil` if it isn't on a US layout.
    static func stroke(for character: Character) -> Stroke? {
        guard let scalar = character.unicodeScalars.first,
              character.unicodeScalars.count == 1 else { return nil }
        return table[scalar].map { Stroke(usage: $0.usage, shift: $0.shift) }
    }

    /// Static lookup table keyed by Unicode scalar. Letters a–z are 0x04–0x1D;
    /// digits 1–9 are 0x1E–0x26 with 0 = 0x27; the rest is US-layout punctuation,
    /// each tagged with whether Left-Shift is held to produce it.
    private static let table: [Unicode.Scalar: (usage: UInt16, shift: Bool)] = {
        var map: [Unicode.Scalar: (usage: UInt16, shift: Bool)] = [:]

        // Letters: lowercase unshifted, uppercase shifted; both hit the same usage.
        for i in 0..<26 {
            let usage = UInt16(0x04 + i)
            let lower = Unicode.Scalar(UInt32(UnicodeScalar("a").value) + UInt32(i))!
            let upper = Unicode.Scalar(UInt32(UnicodeScalar("A").value) + UInt32(i))!
            map[lower] = (usage, false)
            map[upper] = (usage, true)
        }

        // Digit row, unshifted: 1–9 = 0x1E–0x26, 0 = 0x27.
        let digits: [(Character, UInt16)] = [
            ("1", 0x1E), ("2", 0x1F), ("3", 0x20), ("4", 0x21), ("5", 0x22),
            ("6", 0x23), ("7", 0x24), ("8", 0x25), ("9", 0x26), ("0", 0x27),
        ]
        for (ch, usage) in digits { map[ch.unicodeScalars.first!] = (usage, false) }

        // Shifted digit row (US layout): !@#$%^&*() over 1234567890.
        let shiftedDigits: [(Character, UInt16)] = [
            ("!", 0x1E), ("@", 0x1F), ("#", 0x20), ("$", 0x21), ("%", 0x22),
            ("^", 0x23), ("&", 0x24), ("*", 0x25), ("(", 0x26), (")", 0x27),
        ]
        for (ch, usage) in shiftedDigits { map[ch.unicodeScalars.first!] = (usage, true) }

        // Whitespace / control keys reachable from typing.
        let controls: [(Character, UInt16)] = [
            ("\n", returnUsage),   // Return / Enter (also sent on submit)
            ("\r", returnUsage),   // CR maps to Return too
            (" ", 0x2C),           // Space
            ("\t", 0x2B),          // Tab
        ]
        for (ch, usage) in controls { map[ch.unicodeScalars.first!] = (usage, false) }

        // Punctuation keys. Each physical key carries an unshifted and a shifted
        // glyph; both glyphs point at the same usage with the matching shift flag.
        // Pairs are (unshifted, shifted, usage).
        let punctuation: [(Character, Character, UInt16)] = [
            ("-", "_", 0x2D),  // hyphen / underscore
            ("=", "+", 0x2E),  // equals / plus
            ("[", "{", 0x2F),  // left bracket / brace
            ("]", "}", 0x30),  // right bracket / brace
            ("\\", "|", 0x31), // backslash / pipe
            (";", ":", 0x33),  // semicolon / colon
            ("'", "\"", 0x34), // apostrophe / quote
            ("`", "~", 0x35),  // grave / tilde
            (",", "<", 0x36),  // comma / less-than
            (".", ">", 0x37),  // period / greater-than
            ("/", "?", 0x38),  // slash / question
        ]
        for (plain, shifted, usage) in punctuation {
            map[plain.unicodeScalars.first!] = (usage, false)
            map[shifted.unicodeScalars.first!] = (usage, true)
        }

        return map
    }()
}
