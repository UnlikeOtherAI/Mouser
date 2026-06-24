//! macOS input-permission status + prompts.
//!
//! Mouser needs two TCC grants to capture/forward input: **Accessibility** (suppress local
//! input + inject into the target) and **Input Monitoring** (the listen-only `CGEventTap`
//! that senses the cursor reaching the screen edge). The desktop surfaces these so the user
//! isn't left wondering why the cursor won't cross. Caveats: a grant only takes effect after
//! the app restarts, and **re-signing the bundle** (e.g. a dev rebuild) invalidates an
//! existing grant, so it must be re-approved.

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}

/// `kIOHIDRequestTypeListenEvent` — the access class a listen-only event tap needs.
const IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
/// `kIOHIDAccessTypeGranted`.
const IOHID_ACCESS_TYPE_GRANTED: u32 = 0;

/// Whether this process holds the **Accessibility** grant (needed to suppress local input
/// and inject into the peer when controlling it).
#[must_use]
pub fn accessibility_trusted() -> bool {
    // SAFETY: `AXIsProcessTrusted` takes no arguments and returns a bool — always safe.
    unsafe { AXIsProcessTrusted() }
}

/// Whether this process holds the **Input Monitoring** grant (needed by the listen-only
/// edge tap that senses the cursor reaching the screen edge).
#[must_use]
pub fn input_monitoring_trusted() -> bool {
    // SAFETY: `IOHIDCheckAccess` takes a request-type enum and returns an access-type enum.
    unsafe { IOHIDCheckAccess(IOHID_REQUEST_TYPE_LISTEN_EVENT) == IOHID_ACCESS_TYPE_GRANTED }
}

/// Ask the OS to prompt for **Accessibility** (adds the app to the list and opens the pane
/// the first time it's called for an untrusted process). Returns the current trust state.
pub fn prompt_accessibility() -> bool {
    // SAFETY: we build a valid CFDictionary { kAXTrustedCheckOptionPrompt: true } and pass
    // its ref by borrow; the call reads it and returns a bool.
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let value = CFBoolean::true_value();
        let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef())
    }
}

/// Ask the OS to prompt for **Input Monitoring**. Returns whether it is now granted.
pub fn prompt_input_monitoring() -> bool {
    // SAFETY: `IOHIDRequestAccess` takes a request-type enum and returns a bool.
    unsafe { IOHIDRequestAccess(IOHID_REQUEST_TYPE_LISTEN_EVENT) }
}
