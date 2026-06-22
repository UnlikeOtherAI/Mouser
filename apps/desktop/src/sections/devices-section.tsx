import { useState } from "react";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { useWorkspace } from "../lib/use-workspace";
import { osIcon, osLabel, stateMeta } from "../lib/os-meta";
import { cx } from "../lib/cx";
import type { Peer } from "../lib/types";

/** Lists the machines in the workspace — this computer plus the engine's peers. */
export function DevicesSection(): React.JSX.Element {
  // `peers` and `connection` come from the engine over `mouser-ipc` (polled). The
  // engine owns discovery + trust + the live connection; this section reflects that
  // state and drives it with per-peer Connect/Disconnect.
  const {
    devices,
    peers,
    connection,
    localId,
    engineRunning,
    loading,
    connectPeer,
    disconnectPeer,
    trustPeer,
  } = useWorkspace();

  // Tracks the peer whose connect/disconnect/pair request is in flight, to disable
  // buttons, plus the last action error so failures are never silent.
  const [busyPeerId, setBusyPeerId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const runAction = async (
    peerId: string,
    action: () => Promise<void>,
  ): Promise<void> => {
    setBusyPeerId(peerId);
    setActionError(null);
    try {
      await action();
    } catch (e) {
      // Surface the reason instead of swallowing it (the original silent failure).
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyPeerId(null);
    }
  };

  // A failed connection attempt reported asynchronously by the daemon (e.g. the
  // peer hasn't paired back, or is unreachable) — the dial outcome the connect
  // command itself can't return synchronously.
  const connectionError =
    connection.state === "idle" ? connection.error : null;

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted">
        This computer is shown below. Other machines running Mouser on your
        network appear here as the engine discovers them.
      </p>
      <ul className="space-y-2">
        {devices.map((device) => {
          const meta = stateMeta(device.state);
          return (
            <li
              key={device.id}
              className="flex items-center justify-between rounded-xl border border-ink-line bg-ink-card px-4 py-3"
            >
              <div className="flex items-center gap-3">
                <FontAwesomeIcon
                  icon={osIcon(device.os)}
                  aria-hidden="true"
                  className="w-5 text-lg text-fg-strong"
                />
                <div>
                  <p className="text-sm font-semibold text-fg">
                    {device.name}
                  </p>
                  <p className="text-xs text-muted">
                    {osLabel(device.os)} ·{" "}
                    {device.role === "coordinator" ? "This device" : "Member"} ·{" "}
                    {device.monitors.length}{" "}
                    {device.monitors.length === 1 ? "display" : "displays"}
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-2">
                <span
                  aria-hidden="true"
                  className={cx("h-2.5 w-2.5 rounded-full", meta.dot)}
                />
                <span className={cx("text-xs font-medium", meta.text)}>
                  {meta.label}
                </span>
              </div>
            </li>
          );
        })}
        {peers.map((peer) => (
          <PeerRow
            key={peer.id}
            peer={peer}
            connected={
              connection.state === "connected" &&
              connection.peerId === peer.id
            }
            connecting={
              connection.state === "connecting" &&
              connection.peerId === peer.id
            }
            busy={busyPeerId === peer.id}
            onConnect={() => void runAction(peer.id, () => connectPeer(peer.id))}
            onDisconnect={() => void runAction(peer.id, disconnectPeer)}
            onPair={() => void runAction(peer.id, () => trustPeer(peer.id))}
            localId={localId}
          />
        ))}
      </ul>
      {actionError ? (
        <p className="rounded-xl border border-rose-500/40 bg-rose-500/5 px-4 py-3 text-xs text-rose-300">
          {actionError}
        </p>
      ) : null}
      {connectionError ? (
        <p className="rounded-xl border border-rose-500/40 bg-rose-500/5 px-4 py-3 text-xs text-rose-300">
          Connection failed: {connectionError}
        </p>
      ) : null}
      {!engineRunning ? (
        <p className="rounded-xl border border-dashed border-amber-500/40 px-4 py-3 text-xs text-amber-300">
          The Mouser engine is not running. Start the <code>mouserd</code>{" "}
          daemon to discover and connect to other machines.
        </p>
      ) : !loading && peers.length === 0 ? (
        <p className="rounded-xl border border-dashed border-ink-line px-4 py-3 text-xs text-muted">
          No other devices found yet. They appear here once another machine runs
          Mouser on this network.
        </p>
      ) : null}
    </div>
  );
}

interface PeerRowProps {
  peer: Peer;
  connected: boolean;
  connecting: boolean;
  busy: boolean;
  onConnect: () => void;
  onDisconnect: () => void;
  onPair: () => void;
  localId: string | null;
}

/** One discovered peer with its trust/connection status and the right action:
 * Pair (when untrusted), Connect (when trusted), or Disconnect (when connected). */
function PeerRow({
  peer,
  connected,
  connecting,
  busy,
  onConnect,
  onDisconnect,
  onPair,
  localId,
}: PeerRowProps): React.JSX.Element {
  const status = connected
    ? { label: "Connected", dot: "bg-emerald-400", text: "text-emerald-300" }
    : connecting
      ? { label: "Connecting", dot: "bg-amber-400", text: "text-amber-300" }
      : peer.trusted
        ? { label: "Trusted", dot: "bg-sky-400", text: "text-sky-300" }
        : { label: "Not paired", dot: "bg-slate-500", text: "text-muted" };

  return (
    <li className="rounded-xl border border-ink-line bg-ink-card px-4 py-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <FontAwesomeIcon
            icon={osIcon(peer.os)}
            aria-hidden="true"
            className="w-5 text-lg text-fg-strong"
          />
          <div>
            <p className="text-sm font-semibold text-fg">{peer.name}</p>
            <p className="text-xs text-muted">
              {osLabel(peer.os)} · {peer.host}:{peer.port}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-3">
          <span className="flex items-center gap-2">
            <span
              aria-hidden="true"
              className={cx("h-2.5 w-2.5 rounded-full", status.dot)}
            />
            <span className={cx("text-xs font-medium", status.text)}>
              {status.label}
            </span>
          </span>
          {connected ? (
            <button
              type="button"
              disabled={busy}
              onClick={onDisconnect}
              className="rounded-lg border border-ink-line px-3 py-1 text-xs font-medium text-fg hover:bg-ink-line disabled:opacity-50"
            >
              Disconnect
            </button>
          ) : peer.trusted ? (
            <button
              type="button"
              disabled={busy || connecting}
              onClick={onConnect}
              className="rounded-lg border border-sky-500/50 px-3 py-1 text-xs font-medium text-sky-200 hover:bg-sky-500/10 disabled:opacity-50"
            >
              {connecting ? "Connecting…" : "Connect"}
            </button>
          ) : (
            <button
              type="button"
              disabled={busy}
              onClick={onPair}
              className="rounded-lg border border-sky-500/50 px-3 py-1 text-xs font-medium text-sky-200 hover:bg-sky-500/10 disabled:opacity-50"
            >
              {busy ? "Pairing…" : "Pair"}
            </button>
          )}
        </div>
      </div>
      {!peer.trusted && !connected ? (
        <p className="mt-2 text-xs text-muted">
          Pairing is mutual. After you pair here, open Mouser on{" "}
          <span className="font-medium text-fg">{peer.name}</span> and pair this
          device
          {localId ? (
            <>
              {" "}
              (id <code className="text-fg-strong">{localId}</code>)
            </>
          ) : null}{" "}
          before connecting.
        </p>
      ) : null}
    </li>
  );
}
