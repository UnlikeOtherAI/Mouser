# Project Brief: Modern Cross-Platform Multi-Computer Workspace Manager

## Vision

Create a modern replacement for Barrier, Synergy, Logitech Flow, Universal Control, and traditional KVM solutions.

The system treats multiple physical computers as a single logical workspace while remaining fully peer-to-peer, resilient, and platform-native.

Supported platforms:

* macOS
* Windows
* Linux
* Future:

  * Mobile Remote Control App (iOS & Android)

The user should feel as if all connected computers are one large desktop regardless of operating system.

---

# Core Principles

## 1. Zero Configuration

Installation should require:

* Install application
* Launch application
* Machines discover each other automatically

No:

* IP addresses
* Manual server setup
* Port forwarding
* Network configuration

Everything should work automatically on a trusted local network.

---

## 2. Local First

All functionality operates entirely on the local network.

No cloud dependency.

Benefits:

* Near-zero latency
* Privacy
* Offline operation
* No subscriptions
* No vendor lock-in

Cloud services may be added later only for:

* Remote access
* Device synchronization between locations

Never required.

---

## 3. Fault Tolerant

Any device can disappear without breaking the system.

Examples:

* Laptop sleeps
* Desktop shuts down
* WiFi disconnects

Workspace remains operational.

The user should never need to reconnect devices manually.

---

# Architecture

## Device Discovery Layer

Responsible for discovering machines automatically.

Every instance advertises:

* Device name
* OS type
* Device ID
* Capabilities
* Availability
* Version

Examples:

* Mac Studio
* Gaming PC
* Linux Server
* Phone (companion)

Discovery must continuously update.

New devices appear automatically.

Offline devices disappear automatically.

---

# Device Identity

Each machine receives a permanent unique identity.

Identity survives:

* Reboots
* IP changes
* DHCP renewals

Display layouts must reference device identities rather than network addresses.

---

# Cluster Model

All connected machines form a cluster.

The cluster maintains:

* Device list
* Screen arrangement
* Input ownership
* Configuration
* Shared resources

Every device maintains a copy.

No single point of failure.

---

# Leadership System

Avoid terms like:

* Broker
* MQTT
* Master Node

User-facing terminology:

* Server
* Coordinator

---

## Device Roles

Each device can be:

### Eligible

Can become coordinator.

### Ineligible

Never becomes coordinator.

Examples:

* Laptop
* Battery-powered devices
* Temporary devices

User setting:

"Allow this device to act as Coordinator"

Default:

Enabled on desktops.

Disabled on laptops.

---

# Coordinator Election

Purpose:

Maintain cluster consistency.

Responsibilities:

* Conflict resolution
* Device registration
* Configuration authority
* State synchronization

Coordinator is not a dependency.

If coordinator disappears:

* Remaining devices elect replacement
* Cluster continues

Election must be automatic.

User should never see it happen.

---

# Shared Configuration

Every machine stores:

* Screen layout
* Device settings
* Aliases
* Input preferences
* Security permissions

Changes replicate immediately.

Example:

Move Linux monitor to the left of Mac.

All devices instantly receive:

New arrangement.

---

# Workspace Layout

Central visual feature.

---

## Layout Canvas

User sees:

Large gray canvas.

Each connected device appears as:

Rectangle.

Contains:

* Device name
* OS icon
* Connection state
* Device role

Examples:

🪟 Windows

🍎 macOS

🐧 Linux

📱 Phone

---

## Drag Arrangement

User drags devices.

Example:

Linux ← Mac ← Windows

Arrangement immediately updates cluster-wide.

---

## Visual Identification

Clicking device rectangle triggers:

### Arrangement Highlight

Selected rectangle:

* Blue border
* Blue glow

---

### Device Identification Overlay

Actual device displays:

Massive centered number.

Examples:

1

2

3

4

Useful when:

* Multiple identical monitors
* Large installations
* Initial setup

Overlay automatically disappears.

---

# Input Ownership Model

Most important concept.

Only one machine owns keyboard input at a time.

---

## Active Device

Current machine receiving:

* Keyboard
* Mouse
* Shortcuts
* Clipboard operations

All machines know:

Who is active.

---

## Ownership Change Methods

### Mouse Boundary Crossing

Cursor exits one device.

Appears on adjacent device.

Ownership transfers.

---

### Window Interaction

User clicks:

* Window
* Application
* Desktop

Ownership immediately transfers.

Even without cursor boundary crossing.

This feels more natural.

---

### Explicit Hotkeys

Optional:

User-defined shortcuts.

Examples:

* Jump to Mac
* Jump to Linux
* Jump to Windows

---

# Focus Synchronization

All devices maintain awareness of:

Current focus owner.

States:

* Active
* Standby
* Disconnected

Benefits:

* Clipboard routing
* Keyboard routing
* Shared integrations

---

# Clipboard Synchronization

Optional but expected.

Support:

* Text
* Images
* Files

User controls:

* Disabled
* Text only
* Full synchronization

---

# File Transfer

Drag-and-drop between devices.

Examples:

Mac → Windows

Linux → Mac

Windows → Linux

Should feel like moving files between monitors.

---

# Notification System

Coordinator-independent.

Notify:

* Device connected
* Device disconnected
* Configuration changed
* New coordinator elected

Non-intrusive.

---

# Service Architecture

Runs as background service.

---

## macOS

Menu bar application.

---

## Windows

System tray application.

---

## Linux

System tray / desktop environment integration.

---

# Startup Behavior

Options:

### Launch on Login

Default:

Enabled.

---

### Start Minimized

Default:

Enabled.

---

### Install for All Users

Administrator option.

Useful:

Shared workstations.

---

# Security Model

Trust is explicit.

---

## New Device Joining

First connection:

Approval required.

Display:

* Device name
* OS
* Network address

User approves.

---

## Trusted Device List

Remember approved devices.

Future connections:

Automatic.

---

# Permissions

Granular controls.

Per device:

Allow:

* Keyboard
* Mouse
* Clipboard
* File transfer
* Webcam sharing
* Audio sharing

Independent toggles.

---

# Webcam Sharing

Future premium feature.

---

## Objective

Expose a webcam connected to one machine as a webcam on another.

Example:

Mac webcam appears as:

Camera device on Windows.

No screen capture.

No window streaming.

Actual camera feed only.

---

## User Experience

Select:

"Share Webcam"

Choose:

* Built-in camera
* USB camera
* DSLR

Receiving machine sees:

Virtual camera.

Available inside:

* Teams
* Zoom
* Discord
* OBS

---

## Device Routing

Any camera can be exposed to:

* One machine
* Multiple machines

---

# Audio Device Sharing

Future feature.

Expose:

* Microphones
* Speakers

Across devices.

Example:

Mac microphone appears on Windows.

---

# Mobile Companion App

Future mobile application (iOS & Android).

---

## Orientation

Portrait.

The app is designed and operated in portrait orientation.

---

## Layout

Single portrait screen split into two stacked areas:

* Touchpad above.
* Native keyboard below.

---

## Remote Touchpad

Upper screen.

Trackpad.

Controls the cursor on whichever computer currently owns input.

---

## Keyboard

Lower screen.

Native on-screen keyboard.

Uses the platform's built-in keyboard, sending keystrokes to the active computer.

---

## Quick Device Selection

Tap:

Mac

Windows

Linux

Instant ownership transfer.

---

## Bed Mode

Primary purpose:

Operate computers remotely.

No desk required.

---

# Future Workspace Features

## Universal Device Search

Search:

Application

Window

Document

Across all machines.

---

## Unified Clipboard History

Single clipboard history.

Cluster-wide.

---

## Shared Notifications

Optionally forward:

* Teams
* Slack
* Email

Between devices.

---

## Session Persistence

Workspace remembers:

* Layout
* Roles
* Device names
* Permissions

After reboot.

---

# Success Criteria

The finished product should feel like:

* Apple's Universal Control
* Logitech Flow
* Synergy
* Barrier

But:

* Cross-platform
* Modern UI
* Self-healing
* Peer-to-peer
* Multi-device aware
* Webcam-share capable
* Zero configuration
* No cloud dependency
* No single point of failure

The user should stop thinking about individual computers and start thinking about a single workspace composed of multiple machines.
