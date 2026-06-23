import { useEffect, useRef, useState } from "react";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import {
  faSpinner,
  faTriangleExclamation,
} from "@fortawesome/free-solid-svg-icons";
import { useWorkspace } from "../lib/workspace-context";
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
    pairing,
    loading,
    connectPeer,
    connectingPeerId,
    connectFailure,
    dismissConnectFailure,
    cancelConnect,
    disconnectPeer,
    trustPeer,
    approvePairing,
    denyPairing,
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

  // The display name for the failed peer, resolved for the pop-up message.
  const failedPeerName = connectFailure
    ? (peers.find((p) => p.id === connectFailure.peerId)?.name ?? "the device")
    : null;

  return (
    <div className="space-y-3">
      {pairing ? (
        <PairingPrompt
          name={pairing.name}
          sas={pairing.sas}
          onApprove={() => void approvePairing(pairing.peerId)}
          onDeny={() => void denyPairing(pairing.peerId)}
        />
      ) : null}
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
              connectingPeerId === peer.id ||
              (connection.state === "connecting" &&
                connection.peerId === peer.id)
            }
            busy={busyPeerId === peer.id}
            onConnect={() => void runAction(peer.id, () => connectPeer(peer.id))}
            onCancel={() => void cancelConnect()}
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
      {connectFailure ? (
        <ConnectFailureModal
          name={failedPeerName ?? "the device"}
          message={connectFailure.message}
          onRetry={() => {
            const peerId = connectFailure.peerId;
            dismissConnectFailure();
            void connectPeer(peerId);
          }}
          onDismiss={dismissConnectFailure}
        />
      ) : null}
      {!engineRunning ? (
        <p className="rounded-xl border border-dashed border-amber-500/40 px-4 py-3 text-xs text-amber-300">
          The Mouser engine is not responding. Quit and reopen Mouser to
          restart it.
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
  onCancel: () => void;
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
  onCancel,
  onDisconnect,
  onPair,
  localId,
}: PeerRowProps): React.JSX.Element {
  const status = connected
    ? { label: "Connected", dot: "bg-emerald-400", text: "text-emerald-300" }
    : connecting
      ? { label: "Connecting…", dot: "bg-amber-400", text: "text-amber-300" }
      : peer.trusted
        ? { label: "Trusted", dot: "bg-sky-400", text: "text-sky-300" }
        : { label: "Not paired", dot: "bg-slate-500", text: "text-muted" };

  // A non-dialable peer (iport 0) is a controller-only device, e.g. a phone: it
  // can control this machine after pairing, but there is nothing to connect *to*.
  const dialable = peer.port !== 0;

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
              {osLabel(peer.os)} · {peer.host}
              {dialable ? `:${peer.port}` : ""}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-3">
          {/* Live region so screen readers hear connection state changes. */}
          <span
            className="flex items-center gap-2"
            role="status"
            aria-live="polite"
          >
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
            dialable ? (
              connecting ? (
                // While dialing, the action becomes Cancel so the user is never locked
                // watching the spinner until the timeout fires.
                <button
                  type="button"
                  onClick={onCancel}
                  className="inline-flex items-center gap-1.5 rounded-lg border border-ink-line px-3 py-1 text-xs font-medium text-fg hover:bg-ink-line"
                >
                  <FontAwesomeIcon icon={faSpinner} spin aria-hidden="true" />
                  Cancel
                </button>
              ) : (
                <button
                  type="button"
                  disabled={busy}
                  onClick={onConnect}
                  className="rounded-lg border border-sky-500/50 px-3 py-1 text-xs font-medium text-sky-200 hover:bg-sky-500/10 disabled:opacity-60"
                >
                  Connect
                </button>
              )
            ) : (
              <span className="text-xs font-medium text-muted">Controller</span>
            )
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
        <div className="mt-2 rounded-lg border border-ink-line bg-ink px-3 py-2 text-xs text-fg">
          <p>
            Pairing is mutual. Tap <span className="font-semibold">Pair</span>{" "}
            here, then open Mouser on{" "}
            <span className="font-semibold">{peer.name}</span> and pair this
            device back before connecting.
          </p>
          {localId ? (
            <p className="mt-1 text-muted">
              This device&apos;s id:{" "}
              <code className="break-all text-fg-strong">{localId}</code>
            </p>
          ) : null}
        </div>
      ) : null}
    </li>
  );
}

interface ConnectFailureModalProps {
  name: string;
  message: string;
  onRetry: () => void;
  onDismiss: () => void;
}

/** Modal pop-up explaining why a connection attempt failed, instead of the button
 *  silently flicking back to "Connect". Click the backdrop or Dismiss to close. */
function ConnectFailureModal({
  name,
  message,
  onRetry,
  onDismiss,
}: ConnectFailureModalProps): React.JSX.Element {
  const retryRef = useRef<HTMLButtonElement>(null);
  // Move focus into the dialog on open and close on Escape — a `role="dialog"` that
  // can't be dismissed by keyboard / strands focus is an a11y violation.
  useEffect(() => {
    retryRef.current?.focus();
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") onDismiss();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onDismiss]);
  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="connect-failure-title"
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
    >
      {/* Backdrop closes on click; kept out of the tab order (Escape handles keyboard). */}
      <button
        type="button"
        aria-hidden="true"
        tabIndex={-1}
        onClick={onDismiss}
        className="absolute inset-0 bg-black/50"
      />
      <div className="relative w-full max-w-sm rounded-2xl border border-rose-500/40 bg-ink-card p-5 shadow-xl">
        <div className="flex items-center gap-2">
          <FontAwesomeIcon
            icon={faTriangleExclamation}
            aria-hidden="true"
            className="text-rose-300"
          />
          <h2
            id="connect-failure-title"
            className="text-sm font-semibold text-fg"
          >
            Couldn&apos;t connect to {name}
          </h2>
        </div>
        <p className="mt-2 text-xs leading-relaxed text-muted">{message}</p>
        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onDismiss}
            className="rounded-lg border border-ink-line px-3 py-1 text-xs font-medium text-fg hover:bg-ink-line"
          >
            Dismiss
          </button>
          <button
            ref={retryRef}
            type="button"
            onClick={onRetry}
            className="rounded-lg border border-sky-500/50 bg-sky-500/20 px-3 py-1 text-xs font-medium text-sky-100 hover:bg-sky-500/30"
          >
            Try again
          </button>
        </div>
      </div>
    </div>
  );
}

interface PairingPromptProps {
  name: string;
  sas: string;
  onApprove: () => void;
  onDeny: () => void;
}

/** Allow/deny prompt for an untrusted device asking to control this machine. The device's
 *  announced name is shown so the user can recognize it; allowing trusts it (the §3 cert
 *  pin authenticates the specific device, the name is just a human label). */
function PairingPrompt({
  name,
  sas,
  onApprove,
  onDeny,
}: PairingPromptProps): React.JSX.Element {
  return (
    <div className="rounded-xl border border-sky-500/50 bg-sky-500/10 px-4 py-3">
      <p className="text-sm font-semibold text-fg">
        <span className="text-sky-200">{name}</span> wants to control this
        computer
      </p>
      <p className="mt-1 text-xs text-muted">
        Compare this code on both devices before allowing:{" "}
        <span className="font-mono text-sm font-semibold text-fg">
          {sas}
        </span>
      </p>
      <div className="mt-3 flex justify-end gap-2">
        <button
          type="button"
          onClick={onDeny}
          className="rounded-lg border border-ink-line px-3 py-1 text-xs font-medium text-fg hover:bg-ink-line"
        >
          Deny
        </button>
        <button
          type="button"
          onClick={onApprove}
          className="rounded-lg border border-sky-500/50 bg-sky-500/20 px-3 py-1 text-xs font-medium text-sky-100 hover:bg-sky-500/30"
        >
          Allow
        </button>
      </div>
    </div>
  );
}
