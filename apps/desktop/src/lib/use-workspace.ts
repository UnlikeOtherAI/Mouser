import { useCallback, useEffect, useRef, useState } from "react";
import { logDebug } from "./debug-log";
import {
  DEFAULT_ENGINE_SETTINGS,
  type Device,
  type EngineConnection,
  type EngineConnectionState,
  type EngineSettings,
  type HealthItem,
  type OsKind,
  type Peer,
} from "./types";

/** Short id for logs (full ids are long base32 strings). */
function shortId(id: string): string {
  return id.length > 10 ? `${id.slice(0, 8)}…` : id;
}

function errMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

// Shape returned by the `local_device` Tauri command (src-tauri/src/lib.rs).
interface RawMonitor {
  id: string;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
}
interface RawLocalDevice {
  id: string;
  name: string;
  os: string;
  monitors: RawMonitor[];
}

// Shape returned by the `engine_snapshot` Tauri command (src-tauri/src/lib.rs),
// which proxies the engine's `mouser_ipc::Snapshot` over the local IPC link.
interface RawEnginePeer {
  id: string;
  name: string;
  os: string;
  host: string;
  port: number;
  trusted: boolean;
}
interface RawEngineConnection {
  state: string;
  peer_id: string | null;
  owner: string | null;
  epoch: number | null;
  error: string | null;
}
interface RawEnginePairing {
  peer_id: string;
  name: string;
  sas: string;
}
interface RawEngineSnapshot {
  engine_running: boolean;
  local_id: string | null;
  peers: RawEnginePeer[];
  connection: RawEngineConnection;
  pairing: RawEnginePairing | null;
  settings: EngineSettings;
  diagnostics: HealthItem[];
}

/** A pending inbound pairing request awaiting the user's Approve/Deny. */
export interface Pairing {
  peerId: string;
  name: string;
  sas: string;
}

// How often we re-poll the engine snapshot over IPC. The daemon pushes snapshots
// on change, but the UI uses a simple request/reply poll (no event subscription).
const SNAPSHOT_POLL_MS = 2000;
// While a connect is in flight we poll faster so the connected/failed outcome
// surfaces promptly instead of being missed between 2s polls (a dial can fail in a
// fraction of a second).
const CONNECTING_POLL_MS = 350;
// How long to wait for a dial to land before declaring it failed, when the engine
// never reports an explicit error (e.g. the peer is offline / its address moved).
const CONNECT_TIMEOUT_MS = 12000;

/** A connect attempt that failed, surfaced to the user as a dismissible pop-up. */
export interface ConnectFailure {
  peerId: string;
  message: string;
}

// Browser/dev fallback when there is no Tauri runtime (e.g. `pnpm dev` in a
// plain browser). Two screens side by side so snapping is exercised.
const FALLBACK: Device[] = [
  {
    id: "local",
    name: "This Mac",
    os: "macos",
    state: "connected",
    role: "coordinator",
    monitors: [
      { id: "local-mon-0", width: 1512, height: 982, x: 0, y: 0 },
      { id: "local-mon-1", width: 1920, height: 1080, x: 1512, y: -49 },
    ],
  },
];

function osKindOf(os: string): OsKind {
  if (os === "windows") return "windows";
  if (os === "linux") return "linux";
  // The iOS companion advertises `os: "phone"` (controller-only; it publishes its
  // presence for pairing but isn't a dial target). Surface it as a phone so the
  // device list renders the mobile icon/label rather than defaulting to macOS.
  if (os === "phone") return "phone";
  return "macos";
}

function toDevice(raw: RawLocalDevice): Device {
  return {
    id: raw.id || "local",
    name: raw.name || "This computer",
    os: osKindOf(raw.os),
    state: "connected",
    role: "coordinator",
    monitors: raw.monitors.map((m) => ({
      id: m.id,
      width: m.width,
      height: m.height,
      x: m.x,
      y: m.y,
      scale: m.scale,
    })),
  };
}

function toPeer(raw: RawEnginePeer): Peer {
  return {
    id: raw.id,
    name: raw.name || raw.host,
    os: osKindOf(raw.os),
    host: raw.host,
    port: raw.port,
    trusted: raw.trusted,
  };
}

function connectionStateOf(state: string): EngineConnectionState {
  if (state === "connected") return "connected";
  if (state === "connecting") return "connecting";
  return "idle";
}

function toConnection(raw: RawEngineConnection): EngineConnection {
  return {
    state: connectionStateOf(raw.state),
    peerId: raw.peer_id,
    owner: raw.owner,
    epoch: raw.epoch,
    error: raw.error,
  };
}

const IDLE_CONNECTION: EngineConnection = {
  state: "idle",
  peerId: null,
  owner: null,
  epoch: null,
  error: null,
};

export interface Workspace {
  /** The local machine, plus any peers once the engine is wired. */
  devices: Device[];
  /** Peers the engine has discovered on the LAN (with trust), polled over IPC. */
  peers: Peer[];
  /** The engine's current connection state. */
  connection: EngineConnection;
  /** This machine's engine pairing id (base32) the other device must trust. */
  localId: string | null;
  /** True when the daemon's IPC socket is reachable; false means it isn't running. */
  engineRunning: boolean;
  /** A pending inbound pairing request awaiting Approve/Deny, if any. */
  pairing: Pairing | null;
  /** True until the real device query resolves (or falls back). */
  loading: boolean;
  /** Ask the engine to connect to a discovered, trusted peer by id. */
  connectPeer: (peerId: string) => Promise<void>;
  /** The peer a connect is currently in flight to (drives the per-row spinner), or null. */
  connectingPeerId: string | null;
  /** The last connect attempt that failed (drives the failure pop-up), or null. */
  connectFailure: ConnectFailure | null;
  /** Dismiss the connect-failure pop-up. */
  dismissConnectFailure: () => void;
  /** Ask the engine to tear down the current connection. */
  disconnectPeer: () => Promise<void>;
  /** Pair (trust) a discovered peer on this machine by id. */
  trustPeer: (peerId: string) => Promise<void>;
  /** Approve a pending inbound pairing request (trust the peer, accept its connection). */
  approvePairing: (peerId: string) => Promise<void>;
  /** Deny a pending inbound pairing request. */
  denyPairing: (peerId: string) => Promise<void>;
  /** Daemon-owned settings (input/clipboard/security), polled from the engine. */
  settings: EngineSettings;
  /** Update one or more settings (merged over current) — persisted by the daemon. */
  updateSettings: (patch: Partial<EngineSettings>) => Promise<void>;
  /** Connectivity/permission health the engine detected (empty = healthy). */
  diagnostics: HealthItem[];
  /** Trigger a remediation action (e.g. open the relevant OS settings pane). */
  runRemediation: (action: string) => Promise<void>;
}

async function tauriInvoke(): Promise<
  typeof import("@tauri-apps/api/core").invoke | null
> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke;
  } catch {
    return null;
  }
}

/**
 * Loads the real local device (name, OS, physical display layout) and the engine's
 * live state (discovered peers + trust + connection) over the local IPC link.
 *
 * Outside Tauri (browser dev) it falls back to a representative two-screen Mac and an
 * empty/offline engine so the UI is still usable. When the daemon is not running the
 * engine snapshot reports `engine_running: false`, which the UI surfaces as a hint.
 */
export function useWorkspace(): Workspace {
  const [devices, setDevices] = useState<Device[]>(FALLBACK);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [connection, setConnection] = useState<EngineConnection>(IDLE_CONNECTION);
  const [localId, setLocalId] = useState<string | null>(null);
  const [engineRunning, setEngineRunning] = useState(false);
  const [pairing, setPairing] = useState<Pairing | null>(null);
  const [settings, setSettings] = useState<EngineSettings>(DEFAULT_ENGINE_SETTINGS);
  const [diagnostics, setDiagnostics] = useState<HealthItem[]>([]);
  const [loading, setLoading] = useState(true);
  // The peer a connect is in flight to (set on tap, cleared on connected/failed/timeout)
  // and the last failure, so the UI shows a steady spinner then a pop-up — never a flicker.
  const [connectingPeerId, setConnectingPeerId] = useState<string | null>(null);
  const [connectFailure, setConnectFailure] = useState<ConnectFailure | null>(null);
  // Mirror of `connectingPeerId` for the poll loop (which closes over its initial state).
  const connectingRef = useRef<string | null>(null);
  // Last logged engine/connection signatures, so the poll logs transitions only.
  const lastRunning = useRef<boolean | null>(null);
  const lastConnSig = useRef<string | null>(null);
  // When the user just edited a setting, briefly let the optimistic value stand so
  // a poll arriving before the daemon's republish doesn't flicker it back.
  const settingsEditedAt = useRef(0);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const invoke = await tauriInvoke();
        if (invoke === null) return; // browser dev — keep the fallback device
        const raw = await invoke<RawLocalDevice>("local_device");
        if (!cancelled && raw.monitors.length > 0) {
          setDevices([toDevice(raw)]);
        }
      } catch {
        // No Tauri runtime / query failed — keep the fallback device.
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Poll the engine snapshot over IPC. The daemon answers `GetSnapshot` immediately
  // with the current state; if it is not running the command returns an offline
  // snapshot (engine_running: false) rather than throwing.
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | undefined;
    void (async () => {
      const invoke = await tauriInvoke();
      if (invoke === null) return; // browser dev — no engine to poll
      const poll = async (): Promise<void> => {
        try {
          const raw = await invoke<RawEngineSnapshot>("engine_snapshot");
          if (cancelled) return;
          if (lastRunning.current !== raw.engine_running) {
            lastRunning.current = raw.engine_running;
            logDebug(
              raw.engine_running ? "info" : "error",
              raw.engine_running
                ? `engine reachable (${raw.peers.length} peer(s) discovered)`
                : "engine not reachable over IPC",
            );
          }
          const c = raw.connection;
          const sig = `${c.state}|${c.peer_id ?? ""}|${c.error ?? ""}`;
          if (lastConnSig.current !== sig) {
            lastConnSig.current = sig;
            const peer = c.peer_id ? ` peer=${shortId(c.peer_id)}` : "";
            if (c.error) {
              logDebug("error", `connection failed: ${c.error}`);
            } else {
              logDebug("info", `connection: ${c.state}${peer}`);
            }
          }
          setEngineRunning(raw.engine_running);
          setLocalId(raw.local_id);
          setPeers(raw.peers.map(toPeer));
          setConnection(toConnection(raw.connection));
          setPairing(
            raw.pairing
              ? {
                  peerId: raw.pairing.peer_id,
                  name: raw.pairing.name,
                  sas: raw.pairing.sas,
                }
              : null,
          );
          if (Date.now() - settingsEditedAt.current > 2500) {
            setSettings(raw.settings);
          }
          setDiagnostics(raw.diagnostics ?? []);
        } catch {
          // Transient invoke failure — keep the last snapshot.
        }
      };
      // Recursive timeout (not setInterval) so we can poll faster while a connect is
      // in flight — the connected/failed outcome then shows in ~350ms, not up to 2s.
      const schedule = (): void => {
        const delay = connectingRef.current ? CONNECTING_POLL_MS : SNAPSHOT_POLL_MS;
        timer = setTimeout(() => {
          void poll().finally(() => {
            if (!cancelled) schedule();
          });
        }, delay);
      };
      await poll();
      if (!cancelled) schedule();
    })();
    return () => {
      cancelled = true;
      if (timer !== undefined) clearTimeout(timer);
    };
  }, []);

  const connectPeer = useCallback(async (peerId: string): Promise<void> => {
    const invoke = await tauriInvoke();
    if (invoke === null) return;
    // Drive the spinner immediately (independent of the poll) and clear any prior
    // failure, so the user sees steady "Connecting…" — never the old flicker.
    setConnectFailure(null);
    setConnectingPeerId(peerId);
    connectingRef.current = peerId;
    logDebug("info", `connect requested → ${shortId(peerId)}`);
    try {
      await invoke("connect_peer", { peerId });
      logDebug("info", "connect command accepted by engine (dialing…)");
    } catch (e) {
      // The command itself was rejected (engine offline / bad request) — fail now.
      const message = errMessage(e);
      setConnectingPeerId(null);
      connectingRef.current = null;
      setConnectFailure({ peerId, message });
      logDebug("error", `connect command rejected: ${message}`);
    }
  }, []);

  // Keep the poll loop's fast/slow choice in sync with the live connecting state.
  useEffect(() => {
    connectingRef.current = connectingPeerId;
  }, [connectingPeerId]);

  // Resolve a pending connect from the polled snapshot: success once the engine reports
  // it connected, failure on an explicit engine error.
  useEffect(() => {
    if (connectingPeerId === null) return;
    if (connection.state === "connected" && connection.peerId === connectingPeerId) {
      setConnectingPeerId(null);
      return;
    }
    if (connection.state === "idle" && connection.error) {
      setConnectFailure({ peerId: connectingPeerId, message: connection.error });
      setConnectingPeerId(null);
    }
  }, [connection, connectingPeerId]);

  // Backstop: if neither connected nor an explicit error arrives within the window, treat
  // it as failed so the attempt never hangs on a silent spinner. Keyed only on
  // `connectingPeerId` so a routine snapshot poll doesn't keep resetting the timer.
  useEffect(() => {
    if (connectingPeerId === null) return;
    const peerId = connectingPeerId;
    const timer = setTimeout(() => {
      setConnectFailure({
        peerId,
        message:
          "Timed out — the device didn't answer. It may be offline, still starting up, " +
          "or its address changed. Make sure it's running and try again.",
      });
      setConnectingPeerId(null);
    }, CONNECT_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [connectingPeerId]);

  const dismissConnectFailure = useCallback(() => setConnectFailure(null), []);

  const disconnectPeer = useCallback(async (): Promise<void> => {
    const invoke = await tauriInvoke();
    if (invoke === null) return;
    logDebug("info", "disconnect requested");
    try {
      await invoke("disconnect_peer");
    } catch (e) {
      logDebug("error", `disconnect failed: ${errMessage(e)}`);
      throw e;
    }
  }, []);

  const trustPeer = useCallback(async (peerId: string): Promise<void> => {
    const invoke = await tauriInvoke();
    if (invoke === null) return;
    logDebug("info", `pair (trust) requested → ${shortId(peerId)}`);
    try {
      await invoke("trust_peer", { peerId });
      logDebug("info", `paired ${shortId(peerId)} (now trusted on this machine)`);
    } catch (e) {
      logDebug("error", `pair failed: ${errMessage(e)}`);
      throw e;
    }
  }, []);

  const approvePairing = useCallback(async (peerId: string): Promise<void> => {
    const invoke = await tauriInvoke();
    if (invoke === null) return;
    // Optimistically clear the prompt; the next poll reflects the engine's real state.
    setPairing((current) => (current?.peerId === peerId ? null : current));
    await invoke("approve_pairing", { peerId });
  }, []);

  const denyPairing = useCallback(async (peerId: string): Promise<void> => {
    const invoke = await tauriInvoke();
    if (invoke === null) return;
    setPairing((current) => (current?.peerId === peerId ? null : current));
    await invoke("deny_pairing", { peerId });
  }, []);

  const updateSettings = useCallback(
    async (patch: Partial<EngineSettings>): Promise<void> => {
      const next: EngineSettings = { ...settings, ...patch };
      // Optimistic: reflect immediately; the poll guard holds it until the daemon
      // republishes the persisted value.
      settingsEditedAt.current = Date.now();
      setSettings(next);
      const invoke = await tauriInvoke();
      if (invoke === null) return; // browser dev — local-only
      try {
        await invoke("set_settings", { settings: next });
      } catch (e) {
        logDebug("error", `settings update failed: ${errMessage(e)}`);
        throw e;
      }
    },
    [settings],
  );

  const runRemediation = useCallback(async (action: string): Promise<void> => {
    const invoke = await tauriInvoke();
    if (invoke === null) return; // browser dev — nothing to open
    logDebug("info", `remediation requested: ${action}`);
    try {
      await invoke("run_remediation", { action });
    } catch (e) {
      logDebug("error", `remediation "${action}" failed: ${errMessage(e)}`);
      throw e;
    }
  }, []);

  return {
    devices,
    peers,
    connection,
    localId,
    engineRunning,
    pairing,
    loading,
    connectPeer,
    connectingPeerId,
    connectFailure,
    dismissConnectFailure,
    disconnectPeer,
    trustPeer,
    approvePairing,
    denyPairing,
    settings,
    updateSettings,
    diagnostics,
    runRemediation,
  };
}
