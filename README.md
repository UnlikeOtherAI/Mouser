# Mouser

A modern, peer-to-peer replacement for Barrier, Synergy, Logitech Flow, and
Universal Control. Mouser treats multiple physical computers as a single
logical workspace — share one keyboard and mouse across macOS, Windows, and
Linux, move the cursor between machines as if they were one desktop, and sync
the clipboard and files between them.

- **Zero configuration** — install, launch, machines discover each other.
- **Local first** — everything runs on the LAN. No cloud, no accounts.
- **Fault tolerant** — any device can disappear without breaking the cluster.
- **Peer-to-peer** — no broker, no master node, no single point of failure.

## Platforms

| Platform | Status |
|----------|--------|
| macOS    | Planned (menu-bar app) |
| Windows  | Planned (system-tray app) |
| Linux    | Planned (tray / DE integration) |
| iOS / Android companion | Future (portrait: touchpad above, native keyboard below) |

## Documentation

- [docs/brief.md](docs/brief.md) — product brief and vision.
- [docs/architecture.md](docs/architecture.md) — system architecture.
- [docs/tech-stack.md](docs/tech-stack.md) — languages, frameworks, libraries.
- [docs/communication-interface.md](docs/communication-interface.md) — the wire
  protocol that lets independently-built binaries interoperate.

## License

MIT — see [LICENSE](LICENSE).
