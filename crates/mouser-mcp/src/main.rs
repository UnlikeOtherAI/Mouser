//! `mouser-mcp` — a Model Context Protocol (MCP) server exposing Mouser's
//! diagnostics and control surface over stdio, on every platform.
//!
//! It is a thin adapter: every tool drives the running `mouserd` daemon over the
//! same local IPC link the desktop UI uses (`mouser-ipc`), so anything the UI
//! buttons do (inspect state, connect, disconnect, pair/trust, approve/deny an
//! inbound pairing) is also doable programmatically — and the engine log is
//! readable for debugging. The daemon (not the UI process) is the source of truth,
//! so this works headless and identically on macOS, Windows, and Linux.
//!
//! Transport: newline-delimited JSON-RPC 2.0 over stdin/stdout (the MCP stdio
//! transport). No external MCP SDK — the surface is small and tools-only.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use mouser_ipc::{Client, Command, SettingsDto, Snapshot};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// MCP protocol version we advertise when a client doesn't pin one.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
/// Tauri bundle identifier — used to locate the daemon log the desktop captures.
const BUNDLE_ID: &str = "ai.unlikeother.mouser";
/// How long to let an action settle before reading back the resulting snapshot.
const SETTLE: Duration = Duration::from_millis(1200);
/// Tail size for the engine log.
const LOG_TAIL_BYTES: usize = 128 * 1024;

/// Default listen address for the HTTP transport: all interfaces so the same tools
/// are reachable by LAN IP (and discoverable via mDNS), not just loopback.
const DEFAULT_HTTP_ADDR: &str = "0.0.0.0:8765";
/// mDNS service type the HTTP transport advertises so other machines can find it
/// the same way they find the engine itself (`_mouser._udp`).
const MCP_SERVICE_TYPE: &str = "_mouser-mcp._tcp.local.";

#[tokio::main]
async fn main() {
    // Two transports, one tool surface:
    //   * no args            → stdio MCP (local Claude Code / `.mcp.json`)
    //   * `--http [ADDR]`    → Streamable-HTTP MCP on ADDR + mDNS advertisement,
    //                          so the tools are reachable by IP on every platform.
    let args: Vec<String> = std::env::args().collect();
    match args.iter().position(|a| a == "--http") {
        Some(pos) => {
            let addr = args
                .get(pos + 1)
                .filter(|a| !a.starts_with("--"))
                .cloned()
                .unwrap_or_else(|| DEFAULT_HTTP_ADDR.to_string());
            run_http(&addr).await;
        }
        None => run_stdio().await,
    }
}

/// stdio transport: one JSON-RPC message per line (MCP stdio framing).
async fn run_stdio() {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(trimmed) else {
            continue; // ignore non-JSON noise rather than crash the server
        };
        if let Some(response) = handle_message(&request).await {
            if let Ok(text) = serde_json::to_string(&response) {
                if stdout.write_all(text.as_bytes()).await.is_err()
                    || stdout.write_all(b"\n").await.is_err()
                    || stdout.flush().await.is_err()
                {
                    break; // client went away
                }
            }
        }
    }
}

/// Streamable-HTTP transport: serve the same JSON-RPC tools over HTTP and advertise
/// the endpoint over mDNS so any machine on the LAN can discover and reach it.
///
/// Security: a bearer token guards requests. It comes from `MOUSER_MCP_TOKEN` when
/// set; otherwise loopback binds run open (local only) and non-loopback binds get a
/// freshly generated token, printed to stderr for the operator to hand to clients.
/// The token is deliberately NOT placed in the mDNS advertisement.
async fn run_http(addr: &str) {
    let Ok(socket): Result<SocketAddr, _> = addr.parse() else {
        eprintln!("mouser-mcp: invalid --http address {addr:?} (expected HOST:PORT)");
        return;
    };

    let token = resolve_token(&socket);
    let listener = match tokio::net::TcpListener::bind(socket).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("mouser-mcp: could not bind {socket}: {e}");
            return;
        }
    };
    let bound = listener.local_addr().unwrap_or(socket);

    // Advertise over mDNS. Keep the daemon + registration alive for the whole
    // process by holding them until the server returns. `enable_addr_auto` fills the
    // A records from the host's live interfaces (same approach the engine uses).
    let _mdns = advertise(bound.port());

    match &token {
        Some(t) => eprintln!(
            "mouser-mcp: HTTP transport on http://{bound}/mcp (bearer token: {t}); \
             advertising {MCP_SERVICE_TYPE}"
        ),
        None => eprintln!(
            "mouser-mcp: HTTP transport on http://{bound}/mcp (loopback, no token); \
             advertising {MCP_SERVICE_TYPE}"
        ),
    }

    let app = Router::new()
        .route("/", get(health).post(mcp_post))
        .route("/mcp", get(health).post(mcp_post))
        .with_state(Arc::new(token));

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("mouser-mcp: HTTP server stopped: {e}");
    }
}

/// Register the mDNS advertisement for the HTTP transport, returning the live
/// `ServiceDaemon` (dropping it sends the goodbye and stops advertising).
fn advertise(port: u16) -> Option<ServiceDaemon> {
    let daemon = ServiceDaemon::new().ok()?;
    let host = host_name();
    let info = ServiceInfo::new(
        MCP_SERVICE_TYPE,
        &format!("mouser-mcp {host}"),
        &format!("{host}.local."),
        "", // address: filled automatically by enable_addr_auto
        port,
        &[("path", "/mcp"), ("transport", "streamable-http")][..],
    )
    .ok()?
    .enable_addr_auto();
    daemon.register(info).ok()?;
    Some(daemon)
}

/// Health/identity probe for `GET` (clients POST JSON-RPC; a bare GET just confirms
/// the endpoint is a mouser-mcp server).
async fn health() -> &'static str {
    "mouser-mcp"
}

/// Handle one POSTed JSON-RPC message over HTTP, mirroring the stdio dispatch.
async fn mcp_post(State(token): State<Arc<Option<String>>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&token, &headers) {
        return (StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response();
    }
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid JSON-RPC body").into_response();
    };
    match handle_message(&request).await {
        // Streamable-HTTP allows a plain JSON response when there is no stream.
        Some(response) => Json(response).into_response(),
        // Notifications take no reply.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Whether a request carries the required `Authorization: Bearer <token>`. When no
/// token is configured (loopback bind, no env override) every request is allowed.
fn authorized(token: &Option<String>, headers: &HeaderMap) -> bool {
    let Some(expected) = token else {
        return true;
    };
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|got| got == expected)
}

/// Resolve the bearer token for an HTTP bind: explicit `MOUSER_MCP_TOKEN` wins;
/// loopback binds stay open; any other interface gets a generated token.
fn resolve_token(socket: &SocketAddr) -> Option<String> {
    if let Ok(env) = std::env::var("MOUSER_MCP_TOKEN") {
        if !env.is_empty() {
            return Some(env);
        }
    }
    if socket.ip().is_loopback() {
        return None;
    }
    Some(random_token())
}

/// A 128-bit random token rendered as lowercase hex. Falls back to a fixed marker
/// only if the OS RNG is unavailable (effectively never), which still requires the
/// operator to set the header explicitly.
fn random_token() -> String {
    let mut buf = [0u8; 16];
    if getrandom::getrandom(&mut buf).is_err() {
        return "set-MOUSER_MCP_TOKEN".to_string();
    }
    let mut out = String::with_capacity(buf.len() * 2);
    for byte in buf {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Best-effort host label for the mDNS instance name.
fn host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| std::env::var("HOST"))
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "mouser".to_string())
}

/// Dispatch one JSON-RPC message, returning the response to write (or `None` for
/// notifications, which take no reply).
async fn handle_message(request: &Value) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    // Notifications carry no id and expect no response (`?` returns None for them).
    let id = request.get("id").cloned()?;

    match method {
        "initialize" => Some(ok(id, initialize_result(request))),
        "tools/list" => Some(ok(id, json!({ "tools": tool_specs() }))),
        "ping" => Some(ok(id, json!({}))),
        "tools/call" => Some(handle_tools_call(id, request).await),
        other => Some(err(id, -32601, &format!("method not found: {other}"))),
    }
}

fn initialize_result(request: &Value) -> Value {
    let version = request
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION)
        .to_string();
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "mouser-mcp", "version": env!("CARGO_PKG_VERSION") }
    })
}

/// The tool catalog advertised to the MCP client.
fn tool_specs() -> Value {
    let peer_arg = json!({
        "type": "object",
        "properties": {
            "peer_id": { "type": "string", "description": "Base32 device id of the peer" }
        },
        "required": ["peer_id"]
    });
    let no_args = json!({ "type": "object", "properties": {} });
    json!([
        {
            "name": "snapshot",
            "description": "Full engine state from the running mouserd daemon: this device's id, discovered peers with trust + address, the live connection (state/owner/epoch/last error), and any pending inbound pairing (peer + SAS).",
            "inputSchema": no_args
        },
        {
            "name": "engine_log",
            "description": "The mouserd daemon's own diagnostics log (discovery, dials, trust checks, capture-mode transitions) — the best place to see why a connection is failing.",
            "inputSchema": no_args
        },
        {
            "name": "connect",
            "description": "Ask the daemon to connect to (control) a discovered, trusted peer by base32 id. Returns the resulting snapshot; a failed dial shows in connection.error.",
            "inputSchema": peer_arg
        },
        {
            "name": "disconnect",
            "description": "Tear down the current peer connection. Returns the resulting snapshot.",
            "inputSchema": no_args
        },
        {
            "name": "trust",
            "description": "Pair (trust) a discovered peer on THIS machine by base32 id, so the engine allows connecting to/from it. Pairing is mutual — the other device must also trust this one. Returns the resulting snapshot.",
            "inputSchema": peer_arg
        },
        {
            "name": "approve_pairing",
            "description": "Approve a pending inbound pairing request (trust the peer and accept its connection). Use the peer_id from snapshot.pairing after confirming its SAS matches the other device. Returns the resulting snapshot.",
            "inputSchema": peer_arg
        },
        {
            "name": "deny_pairing",
            "description": "Deny a pending inbound pairing request (close the connection, do not trust). Returns the resulting snapshot.",
            "inputSchema": peer_arg
        },
        {
            "name": "get_settings",
            "description": "The daemon's persisted settings (pointer crossing, clipboard, security) — the same values the desktop UI shows.",
            "inputSchema": no_args
        },
        {
            "name": "set_settings",
            "description": "Update one or more daemon settings (partial merge over the current values). Returns the resulting settings.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "cross_at_edges": { "type": "boolean", "description": "Cross to an adjacent device at a shared edge" },
                    "edge_behavior": { "type": "string", "enum": ["instant", "delayed", "locked"] },
                    "wrap_around": { "type": "boolean" },
                    "share_scroll": { "type": "boolean" },
                    "shared_clipboard": { "type": "boolean", "description": "Master clipboard switch" },
                    "clipboard_direction": { "type": "string", "enum": ["bidirectional", "send_only", "receive_only"] },
                    "sync_text": { "type": "boolean" },
                    "sync_images": { "type": "boolean" },
                    "sync_files": { "type": "boolean" },
                    "max_auto_sync_bytes": { "type": "integer", "description": "0 = unlimited" },
                    "prefer_native_apple": { "type": "boolean" },
                    "require_approval": { "type": "boolean", "description": "Require SAS approval for new devices" },
                    "encrypted_only": { "type": "boolean" },
                    "release_on_lock": { "type": "boolean" }
                }
            })
        }
    ])
}

async fn handle_tools_call(id: Value, request: &Value) -> Value {
    let params = request.get("params");
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    match run_tool(name, &args).await {
        Ok(text) => ok(id, tool_content(&text, false)),
        Err(text) => ok(id, tool_content(&text, true)),
    }
}

/// Run a tool, returning human/JSON text on success or an error message.
async fn run_tool(name: &str, args: &Value) -> Result<String, String> {
    match name {
        "snapshot" => snapshot_text().await,
        "engine_log" => read_engine_log(),
        "connect" => {
            let peer_id = arg_peer_id(args)?;
            send_then_snapshot(Command::Connect {
                peer_id,
                host: None,
                port: None,
            })
            .await
        }
        "disconnect" => send_then_snapshot(Command::Disconnect).await,
        "get_settings" => get_settings_text().await,
        "set_settings" => set_settings(args).await,
        "trust" => {
            let peer_id = arg_peer_id(args)?;
            send_then_snapshot(Command::Trust { peer_id }).await
        }
        "approve_pairing" => {
            let peer_id = arg_peer_id(args)?;
            send_then_snapshot(Command::ApprovePairing { peer_id }).await
        }
        "deny_pairing" => {
            let peer_id = arg_peer_id(args)?;
            send_then_snapshot(Command::DenyPairing { peer_id }).await
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

fn arg_peer_id(args: &Value) -> Result<String, String> {
    args.get("peer_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "missing required argument: peer_id".to_string())
}

/// Fetch and pretty-print the current snapshot.
async fn snapshot_text() -> Result<String, String> {
    let mut client = Client::connect().await.map_err(daemon_err)?;
    let snapshot = client.fetch_snapshot().await.map_err(estr)?;
    snapshot_json(&snapshot)
}

/// Send a command, let it settle, then return the resulting snapshot so the caller
/// sees the effect (new trust, connection state, or a dial error).
async fn send_then_snapshot(command: Command) -> Result<String, String> {
    {
        let mut client = Client::connect().await.map_err(daemon_err)?;
        client.send_command(&command).await.map_err(estr)?;
    }
    tokio::time::sleep(SETTLE).await;
    let mut client = Client::connect().await.map_err(daemon_err)?;
    let snapshot = client.fetch_snapshot().await.map_err(estr)?;
    snapshot_json(&snapshot)
}

fn snapshot_json(snapshot: &Snapshot) -> Result<String, String> {
    serde_json::to_string_pretty(snapshot).map_err(estr)
}

/// Return just the current settings.
async fn get_settings_text() -> Result<String, String> {
    let mut client = Client::connect().await.map_err(daemon_err)?;
    let snapshot = client.fetch_snapshot().await.map_err(estr)?;
    serde_json::to_string_pretty(&snapshot.settings).map_err(estr)
}

/// Partial-update settings: read the current values, shallow-merge the provided
/// fields, push the full result via `UpdateSettings`, and return the new settings.
async fn set_settings(args: &Value) -> Result<String, String> {
    let mut client = Client::connect().await.map_err(daemon_err)?;
    let snapshot = client.fetch_snapshot().await.map_err(estr)?;

    let mut merged = serde_json::to_value(&snapshot.settings).map_err(estr)?;
    if let (Some(obj), Some(patch)) = (merged.as_object_mut(), args.as_object()) {
        for (key, value) in patch {
            obj.insert(key.clone(), value.clone());
        }
    }
    let settings: SettingsDto =
        serde_json::from_value(merged).map_err(|e| format!("invalid settings update: {e}"))?;

    {
        let mut writer = Client::connect().await.map_err(daemon_err)?;
        writer
            .send_command(&Command::UpdateSettings { settings })
            .await
            .map_err(estr)?;
    }
    tokio::time::sleep(SETTLE).await;
    let mut reader = Client::connect().await.map_err(daemon_err)?;
    let snapshot = reader.fetch_snapshot().await.map_err(estr)?;
    serde_json::to_string_pretty(&snapshot.settings).map_err(estr)
}

/// Read the tail of the daemon log the desktop captures (the daemon's stderr).
fn read_engine_log() -> Result<String, String> {
    let Some(path) = engine_log_path() else {
        return Err("could not resolve the engine log directory on this OS".to_string());
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let start = bytes.len().saturating_sub(LOG_TAIL_BYTES);
            let tail = bytes.get(start..).unwrap_or(&[]);
            let body = String::from_utf8_lossy(tail);
            Ok(format!("log file: {}\n\n{}", path.display(), body))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(format!(
            "no engine log yet at {} (the desktop app writes it once it launches the daemon)",
            path.display()
        )),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

/// The path the desktop routes the daemon's stderr to (Tauri `app_log_dir`).
fn engine_log_path() -> Option<PathBuf> {
    Some(engine_log_dir()?.join("mouserd.log"))
}

/// The per-OS directory Tauri's `app_log_dir` resolves to for this bundle id.
fn engine_log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        env_path("LOCALAPPDATA")
            .or_else(|| env_path("APPDATA"))
            .map(|base| base.join(BUNDLE_ID).join("logs"))
    }
    #[cfg(target_os = "macos")]
    {
        env_path("HOME").map(|home| home.join("Library").join("Logs").join(BUNDLE_ID))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(base) = env_path("XDG_DATA_HOME") {
            Some(base.join(BUNDLE_ID).join("logs"))
        } else {
            env_path("HOME").map(|home| {
                home.join(".local")
                    .join("share")
                    .join(BUNDLE_ID)
                    .join("logs")
            })
        }
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    {
        None
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

// --- JSON-RPC / MCP envelope helpers ---

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Wrap tool output in the MCP `tools/call` result shape.
fn tool_content(text: &str, is_error: bool) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error
    })
}

fn estr<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Friendlier message when the daemon socket isn't reachable.
fn daemon_err<E: std::fmt::Display>(e: E) -> String {
    format!("mouserd is not reachable over IPC ({e}). Is the Mouser app (or `mouserd`) running?")
}
