# Mouser — Windows build, run & test

How to build, run, and test Mouser on a **Windows 10/11 (x64)** machine. This
doc is the Windows counterpart to the macOS/Linux platform notes and is the
authority for the Windows toolchain, the WebView2 requirement, the per-user
engine model, the UIPI / secure-desktop limits, the optional signed `uiAccess`
helper, packaging, and the acceptance test.

> **Status.** The Windows input path lives in
> [`crates/platform-win`](../crates/platform-win) — a `SendInput` injection
> **skeleton** (`move_cursor`, `button`, `key`, `scroll`) plus the Windows half
> of the Appendix B keymap (HID usage → scancode/VK). The daemon
> (`mouser-engine`) and the Tauri UI (`apps/desktop`) named in
> [tech-stack.md §8](tech-stack.md) are **not in the workspace yet**; the
> commands that mention them below are the intended flow and become runnable as
> those crates land. **What you can run on a Windows box today** is the
> `platform-win` build + `win_inject_demo` acceptance test (§7).
>
> This code has **not** been executed on Windows by the author (no Windows host
> was available). It has been *type-checked and clippy-clean against the real
> `windows` 0.62 bindings* by cross-checking with the `x86_64-pc-windows-gnu`
> target on macOS (see §8). Real end-to-end execution is the job of whoever runs
> this on the Windows box — §7 is that test.

---

## 1. Prerequisites

| Component | What / why |
|-----------|------------|
| **Windows 10 1809+ or Windows 11, x64** | `SendInput`, virtual-desktop metrics, WebView2 all assume a modern desktop SKU. ARM64 is out of scope for now. |
| **Visual Studio Build Tools 2022** (or full VS 2022) with the **"Desktop development with C++"** workload | Provides the **MSVC** linker (`link.exe`), the Windows SDK, and CRT headers that the default Rust `*-msvc` toolchain links against. The C++ workload is required — the .NET-only install does **not** include `link.exe`. |
| **Rust via `rustup`** | We pin a stable channel in [`rust-toolchain.toml`](../rust-toolchain.toml). Do **not** use a distro/winget Rust that bypasses rustup. |
| **WebView2 Runtime** (Evergreen) | Tauri's webview backend on Windows (§3). Ships on Win11 and current Win10; the installer bundles the bootstrapper so end users without it are covered. Not needed to build/run `platform-win` alone. |
| **Git** | Clone the repo. |
| (optional) **`cargo-binstall`** | Faster install of `tauri-cli` / `cargo-wix` than building from source. |

### 1.1 Install the toolchain

```powershell
# 1. MSVC build tools (C++ workload). Either install Visual Studio 2022 with
#    "Desktop development with C++", or the standalone Build Tools:
winget install --id Microsoft.VisualStudio.2022.BuildTools --override `
  "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

# 2. rustup (installs the stable-x86_64-pc-windows-msvc toolchain by default):
winget install --id Rustlang.Rustup
#    New shell, then verify the MSVC host triple is the default:
rustup show          # default host should be x86_64-pc-windows-msvc
rustc --version

# 3. WebView2 Evergreen runtime (only needed for the Tauri UI):
winget install --id Microsoft.EdgeWebView2Runtime
```

`rust-toolchain.toml` auto-selects the pinned **stable** channel with `clippy`
and `rustfmt` the first time you run `cargo` in the repo, so you don't pick a
toolchain manually.

> **MSVC vs GNU.** Use the default **`x86_64-pc-windows-msvc`** target on
> Windows — it links against the system CRT and is what we ship. The
> `*-windows-gnu` target is used only as a *no-linker type-check* on non-Windows
> CI (§8); don't build releases with it.

---

## 2. Clone & build the Windows input crate

```powershell
git clone https://github.com/UnlikeOtherAI/Mouser
cd Mouser

# Build just the Windows input skeleton:
cargo build -p platform-win

# Unit tests (the Appendix B keymap table; pure logic, runs anywhere):
cargo test -p platform-win

# Lints must be clean with warnings denied:
cargo clippy -p platform-win --all-targets -- -D warnings
cargo fmt --check
```

On Windows the `windows` crate compiles for real here (on macOS/Linux only the
crate's small non-Windows stub compiles — the heavy `windows-rs` code is
`#[cfg(target_os = "windows")]`-gated, see
[`crates/platform-win/src/lib.rs`](../crates/platform-win/src/lib.rs)).

---

## 3. WebView2 (for the Tauri UI)

The Mouser UI is **Tauri v2** (tech-stack §5). On Windows Tauri renders through
**WebView2** (the Chromium-based Edge runtime), not a bundled browser. Two
things follow:

1. **Build host** needs the WebView2 *SDK* — pulled in automatically by Tauri's
   crates; you only need the **runtime** installed to *run* the UI (step 1.1).
2. **End users** need the WebView2 runtime present. We bundle the **Evergreen
   Bootstrapper** so the installer fetches/installs it on first run if missing.
   In `apps/desktop/src-tauri/tauri.conf.json`:

   ```jsonc
   {
     "bundle": {
       "windows": {
         "webviewInstallMode": { "type": "downloadBootstrapper" }
       }
     }
   }
   ```

   `downloadBootstrapper` keeps the installer small and always installs the
   latest WebView2. (`embedBootstrapper` and `offlineInstaller` are alternatives
   for locked-down/offline fleets; `fixedRuntime` pins a specific version at the
   cost of a much larger package. Evergreen bootstrapper is the default.)

> The `webviewInstallMode` key only takes effect once `apps/desktop` exists; it
> is documented here so the packaging story is settled up front.

---

## 4. Building the engine and the UI

> Forward-looking: runnable once `mouser-engine` and `apps/desktop` land
> (tech-stack §8).

```powershell
# Headless daemon (the cluster member; owns input capture/injection + transport):
cargo build -p mouser-engine --release
#   -> target\release\mouser-engine.exe

# Tauri desktop UI (tray + settings; talks to the engine over a named pipe):
#   Node + pnpm for the frontend, then the Tauri CLI:
winget install --id OpenJS.NodeJS.LTS
corepack enable; corepack prepare pnpm@latest --activate
cargo install tauri-cli --locked      # or: cargo binstall tauri-cli

cd apps\desktop
pnpm install
pnpm tauri dev                        # dev run (hot reload)
pnpm tauri build                      # release bundle (see §6)
```

The UI links **`mouser-ipc`** (typed DTOs), never `mouser-core` directly
(architecture §6), and connects to the engine over a **named pipe** secured by a
pipe ACL (architecture §3).

---

## 5. Running the engine — per-user, NOT a Session 0 service

**Run `mouser-engine` as a per-user process that autostarts on login** (a
per-user scheduled task or a service that runs in the *interactive* session) —
**not** as a classic `LocalSystem` Windows service in **Session 0**. This is a
hard requirement, not a preference:

- **Session 0 isolation.** Since Windows Vista, services run in **Session 0**,
  which has **no interactive desktop**. Synthetic input from a Session 0 process
  does **not** reach the user's interactive desktop (Session 1+). A Session 0
  daemon literally cannot move the user's cursor or type into their apps — the
  whole point of Mouser.
- **`SendInput` targets the caller's desktop/session.** Injection lands on the
  window station + desktop the calling thread is attached to. To drive the
  logged-in user's screen the injector must live **in that user's interactive
  session**.
- **Per-user trust & config.** Identity keys, the trusted-peer list, and
  permissions are **per-user** local state (communication-interface §9). A
  machine-wide `LocalSystem` service would blur whose cluster membership and
  whose grants are in play. One engine **per interactive user** keeps authority
  local to that user.
- **No elevation by default.** Running in the user's session at the user's
  integrity level is the least-privilege default. Elevated injection is opt-in
  via the separate `uiAccess` helper (§6.2), never by making the engine
  `LocalSystem`.

### 5.1 Autostart (per-user scheduled task)

Register a logon-triggered task that runs in the interactive session at the
user's privilege level:

```powershell
$exe = "$env:LOCALAPPDATA\Mouser\mouser-engine.exe"
$action  = New-ScheduledTaskAction  -Execute $exe
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
# Interactive token, NON-elevated (RunLevel Limited):
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME `
  -LogonType Interactive -RunLevel Limited
$settings  = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries `
  -DontStopIfGoingOnBatteries -StartWhenAvailable
Register-ScheduledTask -TaskName "Mouser Engine" -Action $action `
  -Trigger $trigger -Principal $principal -Settings $settings
```

(The installer registers this automatically; the snippet is for manual setup and
debugging. `HKCU\...\Run` is a simpler alternative but a scheduled task gives
restart-on-failure and battery policy.)

---

## 6. Packaging (.msi / .exe)

Both formats are first-class (tech-stack §8). They are produced by the Tauri
bundler from `apps\desktop`:

```powershell
cd apps\desktop
pnpm tauri build
#   .msi  -> WiX-based installer  (target\release\bundle\msi\*.msi)
#   .exe  -> NSIS setup           (target\release\bundle\nsis\*-setup.exe)
```

Select formats explicitly with `pnpm tauri build --bundles msi,nsis`. Both embed
the **WebView2 download bootstrapper** (§3) and register the per-user autostart
task (§5.1) for the installing user.

- **`.msi` (WiX)** — best for enterprise/MDM deployment (Group Policy, Intune).
- **`.exe` (NSIS)** — friendlier per-user install, smaller, supports updater
  hooks.

For a standalone CLI/engine MSI without Tauri, **`cargo-wix`**
(`cargo install cargo-wix`; `cargo wix -p mouser-engine`) produces a WiX MSI
directly — useful for a headless/server install.

### 6.1 Signing (separate workstream)

Code signing is a **separate, gated workstream** (tech-stack §8). Unsigned
installers trip **SmartScreen** ("Windows protected your PC") and scare users.
Release builds must be signed with an **EV (or OV) Authenticode** certificate
via `signtool` (e.g. `signtool sign /fd SHA256 /tr <rfc3161-ts-url> /td SHA256
/a <artifact>`), wired into the protected release CI job — not into PR builds.
The `uiAccess` helper (§6.2) **must** be signed; an unsigned `uiAccess` binary
will not load (§6.2).

### 6.2 Optional signed `uiAccess` helper (off by default)

`SendInput` from a normal (medium-integrity, non-`uiAccess`) process **cannot**
drive a **higher-integrity** window — see §6.3 (UIPI). The escape hatch is a
small, **separate**, signed helper process that carries the **`uiAccess="true"`**
manifest flag, which lets it bypass UIPI and inject into elevated apps **without
running as full admin**. It is **off by default** (architecture §3) and gated
behind an explicit user opt-in.

Windows imposes hard requirements on a `uiAccess` binary — all must hold or it
silently won't get UIAccess:

1. **Authenticode-signed** by a trusted cert (self-signed won't do on a normal
   machine; see §6.1).
2. Installed in a **trusted secure location** — under `%ProgramFiles%` or
   `%SystemRoot%\System32` (a per-user `%LOCALAPPDATA%` path is **not** trusted
   for UIAccess; the helper is the one component that installs to Program Files).
3. App manifest declares:
   ```xml
   <requestedExecutionLevel level="asInvoker" uiAccess="true" />
   ```
4. Either the machine has UAC enabled with the default
   `ValidateAdminCodeSignatures`/secure-desktop policy, or (dev only) the local
   policy **"User Account Control: Only elevate UIAccess applications that are
   installed in secure locations"** governs the trusted-location check.

The engine launches this helper **only** when the user enables "control elevated
apps", hands it input over the same access-controlled IPC, and treats its
absence as `inject = secure_context` for elevated targets rather than failing.

> Even a `uiAccess` helper **cannot** inject into the **secure desktop** (UAC
> prompt / lock screen) — see §6.3. `uiAccess` lifts UIPI, not the secure-desktop
> boundary.

### 6.3 UIPI, secure desktop & lock screen — limits to expect

These are OS boundaries, not bugs. The adapter detects them, broadcasts a
`CapabilityState` (communication-interface §7.4), and **returns ownership to the
source** (it never silently no-ops):

| Boundary | What happens | Wire state |
|----------|--------------|------------|
| **UIPI** (User Interface Privilege Isolation) | A medium-integrity process's `SendInput` is **silently dropped** for windows owned by a higher-integrity (elevated/admin) process. `SendInput` still returns a non-zero count — there is **no error code**. | `inject = secure_context`, `BlockedReason = permission` until the `uiAccess` helper (§6.2) is enabled. |
| **UAC secure desktop** (the dimmed consent prompt) | Runs on a **separate desktop** (`Winlogon`) that no ordinary or even `uiAccess` process can post input to. Injection is impossible while it's up. | `inject = secure_context`, `BlockedReason = secure_desktop`. |
| **Lock screen / Winlogon** | Also a separate secure desktop; no injection. Mouser is **local-only** there. | `inject = secure_context`, `BlockedReason = lock_screen`. |
| **No virtual desktop / zero metrics** | `move_cursor` returns `InjectError::NoVirtualDesktop` (rare; pre-display init). | n/a (transient error). |

Because a *full* `SendInput` success count does **not** prove the input landed
(UIPI/secure desktop swallow accepted events), the engine confirms effect
out-of-band (e.g. expected focus/cursor change) before reporting `available`.

---

## 7. Acceptance test (run this on the Windows box)

This is the concrete end-to-end check the operator must perform on real Windows.
It exercises **cursor motion + a keystroke into Notepad** via the actual
`SendInput` path.

```powershell
# From the repo root:
cargo build -p platform-win
```

Then:

1. Open **Notepad** and **click inside it** so it has keyboard focus. Keep it
   on the primary monitor. (Notepad is a normal medium-integrity app, so UIPI
   does not block it — that's why it's the target.)
2. Run the demo:
   ```powershell
   cargo run -p platform-win --example win_inject_demo
   ```
3. **Expected:**
   - The mouse cursor visibly traces a **square** through four corners of the
     virtual desktop.
   - The characters **`hi`** appear in Notepad.
   - The program prints `RESULT: cursor_moved=yes` and exits `0`.

**If it fails:**

- `RESULT: cursor_moved=no` / exit code `2` → the cursor did not move. Likely the
  foreground window is **elevated** (run Notepad non-elevated), or you are on the
  **secure desktop / lock screen** (§6.3). Try with a non-elevated foreground
  app and the session unlocked.
- Cursor moves but **`hi` does not appear** → Notepad didn't have focus, or the
  target app is higher-integrity than the demo (UIPI). Click into a plain
  Notepad window and retry. To inject into elevated apps you need the signed
  `uiAccess` helper (§6.2).
- `move_cursor failed: virtual desktop has zero size` → no display metrics
  available (headless/RDP session without a desktop); run on a real interactive
  session.

> The demo deliberately uses **scancodes** (physical-key semantics, §7.5 of the
> wire spec), so `hi` is produced by the *physical* H and I keys regardless of
> the active keyboard layout.

Once `mouser-engine` + `apps/desktop` exist, the **full** acceptance test is:
pair two engines (one macOS/Linux, one Windows), cross the screen edge onto the
Windows machine, and type into Notepad from the other keyboard — verifying the
same injection path end-to-end over the cluster.

---

## 8. How this was verified without a Windows machine

The `platform-win` crate could not be *run* on the (macOS) dev host, but its
Windows code was compiled and linted against the **real `windows` 0.62
bindings**, catching API mismatches that a stub alone would hide:

```bash
# On the macOS/Linux dev host (no MSVC linker needed — `check`/`clippy` only):
rustup target add x86_64-pc-windows-gnu

# Type-checks the SendInput/GetCursorPos/GetSystemMetrics code + the example:
cargo check  -p platform-win --all-targets --target x86_64-pc-windows-gnu

# Lints the unsafe Win32 code with warnings denied:
cargo clippy -p platform-win --all-targets --target x86_64-pc-windows-gnu -- -D warnings
```

The macOS/Linux **stub** path is also verified normally:

```bash
cargo build  -p platform-win                       # stub compiles
cargo test   -p platform-win                       # Appendix B keymap tests
cargo clippy -p platform-win --all-targets -- -D warnings
cargo build  --workspace                           # no regression
```

This proves the Windows code **compiles and lints** against the true bindings;
it does **not** prove runtime behaviour on Windows — that is what §7 is for. Run
§7 on a Windows box before declaring Windows support working.
