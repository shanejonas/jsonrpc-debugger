---
name: jsonrpc-debugger
description: Control a running jsonrpc-debugger TUI or transparent stdio wrapper through its localhost JSON-RPC control plane. Use to inspect live state or durable history, send or intercept requests in driver mode, navigate visible panels, annotate exact lines, or verify HTTP, MCP, LSP, and other framed JSON-RPC flows.
---

# JSON-RPC Debugger

Drive the live debugger or inspect a transparent wrapper. Treat an attached TUI as a shared screen: preserve unrelated state and leave it usable.

## Connect

1. Find the existing debugger process and control port. HTTP driver mode defaults to the proxy port plus one. Transparent wrappers usually set `--control-port` explicitly.
2. Probe the control endpoint with `rpc.discover`. Do not confuse it with the proxy port.
3. Read `debugger.getState` before changing anything. Record its session, target, filter, focus, selection, annotations, mode, pending count, and revision.

Read `getState.dataPlane` before acting:

- `http` means the debugger drives the target. `proxyPort` contains its HTTP ingress.
- `stdio` means a transparent wrapper. `proxyPort` is null and the external client owns request IDs while the wrapper can pause and resolve its requests.

`getState.transport` identifies the target wire format. Stdio uses `stdio-json-lines` or `stdio-content-length`. Its command comes from `getState.target` and cannot change through the control plane.

Drive the existing live process when its control endpoint responds. Do not start another debugger unless the user asks.

Use the debugger's generic JSON-RPC client:

```bash
CONTROL_URL=http://127.0.0.1:8081
rpc() {
  local method="$1" params="${2-}"
  if [ -z "$params" ]; then params='{}'; fi
  jsonrpc-debugger call --transport=http "$CONTROL_URL" \
    "$(printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"%s\",\"params\":%s}' "$method" "$params")"
}

rpc rpc.discover
rpc debugger.getState
rpc debugger.setFocus '{"panel":"history"}'
```

`call` prints the complete JSON-RPC response. Treat an `error` envelope as failure even when HTTP returns 200. The runtime OpenRPC document returned by `rpc.discover` is the authority for methods and parameters.

## Debug MCP From Either Side

MCP uses JSON-RPC over newline-delimited stdio or Streamable HTTP. Choose the debugger role from `getState`, not from the command name:

- `dataPlane: "http"` with `transport: "stdio-json-lines"` is driver mode. The debugger acts as the MCP client and `debugger.sendRequest` may send messages.
- `dataPlane: "stdio"` with `transport: "stdio-json-lines"` is wrapper mode. A real client owns the connection; inspect or intercept its messages, but never inject one.
- `dataPlane: "http"` with `transport: "http"` is HTTP proxy mode. Send Streamable HTTP requests through `proxyPort` with their MCP headers intact.

### Identify the protocol era

MCP `2026-07-28` and later is stateless. There is no `initialize` or `notifications/initialized` handshake and no `Mcp-Session-Id`. Every client request carries these fields in `params._meta`:

- `io.modelcontextprotocol/protocolVersion` is required.
- `io.modelcontextprotocol/clientCapabilities` is required and should contain only capabilities relevant to that request.
- `io.modelcontextprotocol/clientInfo` is recommended.

`server/discover` is an optional first call for modern clients and mandatory for modern servers. A dual-era stdio client should use it as a probe before any other request:

```json
{
  "jsonrpc": "2.0",
  "id": "discover-1",
  "method": "server/discover",
  "params": {
    "_meta": {
      "io.modelcontextprotocol/protocolVersion": "2026-07-28",
      "io.modelcontextprotocol/clientCapabilities": {},
      "io.modelcontextprotocol/clientInfo": {
        "name": "jsonrpc-debugger",
        "version": "1.0.0"
      }
    }
  }
}
```

Interpret the probe carefully:

- A `DiscoverResult` means modern MCP. Select a version from `supportedVersions` and make direct calls with per-request `_meta`.
- A recognized modern error such as `UnsupportedProtocolVersionError` means modern MCP. Retry with one of its advertised versions; do not initialize.
- Any other error or a reasonable timeout means legacy MCP over stdio. Fall back to the `initialize` handshake.

MCP `2025-11-25` and earlier is legacy. For legacy traffic only: send `initialize`, read the negotiated version and capabilities, send `notifications/initialized`, then call advertised methods. In wrapper mode, determine the era from the real client's traffic and do not add a second handshake.

### Act as the MCP client

Start the server behind the HTTP driver:

```bash
jsonrpc-debugger --port 8080 stdio -- npx -y @modelcontextprotocol/server-everything
```

For stdio, send the `server/discover` probe through `debugger.sendRequest`. Continue directly with modern calls, or use the legacy handshake only when the probe identifies a legacy server. A modern tool call looks like:

```json
{
  "jsonrpc": "2.0",
  "id": "tool-1",
  "method": "tools/call",
  "params": {
    "_meta": {
      "io.modelcontextprotocol/protocolVersion": "2026-07-28",
      "io.modelcontextprotocol/clientCapabilities": {},
      "io.modelcontextprotocol/clientInfo": {
        "name": "jsonrpc-debugger",
        "version": "1.0.0"
      }
    },
    "name": "echo",
    "arguments": {"message": "hello"}
  }
}
```

Common discovery calls are `tools/list`, `resources/list`, `resources/templates/list`, and `prompts/list`. Exercise concrete operations such as `tools/call`, `resources/read`, and `prompts/get` with unique request IDs.

Modern results must include `resultType`. `complete` is final; `input_required` is a Multi Round-Trip Request that the client answers by retrying the original request with `inputResponses` and the returned `requestState`. Modern servers do not initiate JSON-RPC requests. Legacy servers may; in wrapper mode, let the external client answer them.

MCP tool failures often arrive inside a successful JSON-RPC result as `result.isError: true`; count them separately from JSON-RPC `error` envelopes. Correlate `notifications/progress` with the originating call through `_meta.progressToken`.

### Send Streamable HTTP correctly

For MCP `2026-07-28`, send each JSON-RPC message as its own POST through the debugger proxy. Include:

- `Content-Type: application/json`
- `Accept: application/json, text/event-stream`
- `MCP-Protocol-Version`, matching `params._meta.io.modelcontextprotocol/protocolVersion`
- `Mcp-Method`, matching the JSON-RPC `method`
- `Mcp-Name` for `tools/call`, `resources/read`, and `prompts/get`, matching `params.name` or `params.uri`
- Any `Mcp-Param-{Name}` required by `x-mcp-header` annotations in the selected tool's `inputSchema`

The body is the source of truth. Modern servers reject missing or mismatched mirrored headers with HTTP 400 and JSON-RPC error `-32020` (`HeaderMismatch`). Header names are case-insensitive; method and name values are case-sensitive.

`debugger.sendRequest` cannot set custom HTTP headers. Use an HTTP client against `proxyPort` when driving Streamable HTTP, and then inspect the captured body and headers through the control plane. For example:

```bash
curl -fsS http://127.0.0.1:8080/mcp \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -H 'mcp-protocol-version: 2026-07-28' \
  -H 'mcp-method: tools/call' \
  -H 'mcp-name: echo' \
  --data '{"jsonrpc":"2.0","id":"tool-1","method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/clientInfo":{"name":"jsonrpc-debugger","version":"1.0.0"}},"name":"echo","arguments":{"message":"hello"}}}'
```

Configure the debugger target as the MCP server origin so the local `/mcp` path maps to its `/mcp` endpoint. The proxy forwards MCP headers. Stdio has no header layer: method, name, protocol version, capabilities, and client identity stay in the JSON-RPC body.

The HTTP proxy currently handles ordinary JSON responses, not request-scoped SSE streams. If a target selects `text/event-stream`, use the captured request for header/body inspection but do not treat the debugger's response parse failure as a target MCP failure.

Do not confuse routing headers with MCP cache freshness. `Mcp-Method` and `Mcp-Name` let intermediaries distinguish traffic without parsing the body. Cacheable results such as `server/discover`, `tools/list`, `prompts/list`, `resources/list`, `resources/templates/list`, and `resources/read` carry `ttlMs` and `cacheScope`; stable tool ordering improves prompt-cache hits.

### Observe a real MCP client

Replace the client's MCP server command with a transparent wrapper:

```json
{
  "command": "jsonrpc-debugger",
  "args": [
    "--control-port", "8096",
    "wrap", "--",
    "npx", "-y", "@modelcontextprotocol/server-everything"
  ]
}
```

Open the shared TUI separately:

```bash
jsonrpc-debugger attach http://127.0.0.1:8096
```

The wrapper must launch with the MCP connection; it cannot splice into an existing stdio pipe. Use a unique control port per MCP server. `Ctrl-B p` pauses client-originated requests before the server receives them; `a`, `e`, `c`, `b`, and `r` allow, edit, complete, block, or resume. Server messages continue flowing to avoid deadlocks. Do not complete an idless notification because it has no response.

For modern traffic, verify every client request has the required per-request `_meta`; there will be no HTTP headers in a stdio wrapper. For legacy traffic, verify initialization precedes normal operations and inspect any server-initiated requests without answering on the real client's behalf.

## Inspect Before Acting

- Use `debugger.getHistory` for recent traffic. Pass `sessionId` to inspect an older session without changing the TUI.
- Use `debugger.find` for a case-insensitive literal search across durable session metadata, complete exchanges, and annotations. It searches every session by default; pass `sessionId` to scope it. `limit` applies to each result group.
- Exchange hits include `references`; annotation hits include `reference`. Pass one directly as `debugger.revealLines` params to select its session, exchange, panel, and tab, then highlight the exact line.
- Use `debugger.listSessions` when the relevant traffic may be from an earlier run.
- Use `debugger.waitForChange` with the last revision instead of polling.
- Use `debugger.getPending` before touching interception state.
- Never resolve a pending request you did not create unless the user explicitly asks.
- For inspection requests, report the evidence without mutating the TUI.

## Drive the Shared View

Use `debugger.selectExchange`, `debugger.setFocus`, `debugger.setFilter`, and `debugger.scrollPanel` to show the user what matters. Clear temporary filters afterward.

Use `debugger.setFullscreen` to expand or restore the focused panel. Set focus first, then pass the desired `fullscreen` boolean. Read the current state from `debugger.getState.fullscreen`.

When the user says “this line” or “the selected line,” read `debugger.getState.lineSelection`. It contains the panel, one-based line range, and exact text.

To point at evidence:

1. Read numbered text with `debugger.getPanel`.
2. Find the exact request or response lines.
3. Add a durable note with `debugger.annotateLines`.
4. Call `debugger.revealLines` only when you intend to focus, center, and highlight that evidence for the user.

`debugger.annotateLines` does not select, focus, scroll, switch tabs, or highlight. Pass `exchangeIndex` and `tab` for background annotations. `debugger.revealLines` accepts optional `sessionId`, `exchangeIndex`, and `tab` when you need to navigate before highlighting. Messages must be one line and at most 160 characters. Remove only annotations you created, using their returned ID with `debugger.removeAnnotation`.

Use `debugger.sendRequest` only when `getState.dataPlane` is `http`. It sends a complete target JSON-RPC request through the driver proxy. Keep human-facing request IDs unique, semantic, and at most 12 characters.

Stdio driver requests time out after 120 seconds by default. `stdio --request-timeout <SECONDS>` changes the limit. A `-32603` response containing `stdio request timed out` comes from the debugger, not the target.

Never inject requests into a transparent stdio wrapper. The external MCP/LSP client owns response routing. Use `debugger.getHistory`, `debugger.waitForChange`, or `jsonrpc-debugger attach` to observe it, and use interception to pause or resolve client-originated requests.

## Run Dense Audits

1. Freeze the range and calculate the expected interval count before creating traffic.
2. Reuse a suitable durable session, or create one clearly named session when the user asks for a fresh run.
3. Send intervals oldest-to-newest, one at a time. Retry failures with backoff before advancing.
4. Inspect and annotate each completed response without selecting exchanges or changing focus, filter, scroll, tabs, or highlights.
5. Verify coverage, unique IDs, target JSON-RPC errors, and pending count.
6. Tell the user the audit is ready, rank the interesting findings, then reveal them one at a time during a guided walkthrough.

Do not visually select findings during the background audit. Persistent annotations and temporary highlights are separate tools.

## Intercept Requests

Interception works in HTTP driver mode and transparent stdio wrapper mode. In a wrapper, only client-originated requests pause; server messages continue flowing to avoid deadlocks.

### Driver mode

Interception requires concurrent calls:

1. Call `debugger.setPaused` with `paused: true`.
2. Start `debugger.sendRequest` without awaiting it.
3. Wait for a new revision, then read `debugger.getPending` until that request appears.
4. Resolve its internal pending `id` with `debugger.resolvePending` using `allow`, `block`, or `complete`.
5. Await the original send call.
6. Disable pause and verify the pending count returns to zero.

Use `allow` to forward the original or a replacement request, `block` for a debugger-generated error, and `complete` for a supplied response without forwarding.

### Wrapper mode

For a transparent wrapper, call `debugger.setPaused`, wait for the external client to create a pending request, then resolve it through the same `debugger.getPending` and `debugger.resolvePending` methods. Do not call `debugger.sendRequest`.

## Work With Sessions

History and annotations survive restarts in `~/.config/jsonrpc-debugger/sqlite.db` by default. `XDG_CONFIG_HOME` and `JSONRPC_DEBUGGER_CONFIG_DIR` can move it.

- `debugger.listSessions` lists durable sessions newest first.
- `debugger.getHistory` reads a session without selecting it and supports `limit` and `before` pagination.
- `debugger.selectSession` makes a session visible and restores its target.
- `debugger.createSession` creates and selects an empty session. Do not create one merely to inspect history.
- `debugger.renameSession` renames a session without selecting it.
- `debugger.exportSession` returns portable JSON.
- `debugger.replaySession` appends portable history without forwarding requests.

Session changes fail while intercepted requests are pending.

## Finish Cleanly

- Resolve every request you created.
- Restore pause, target, and temporary filter state when changed for the task.
- Preserve user-created selections and annotations.
- Preserve history unless the user explicitly requests deletion.
- Report the final session, mode, pending count, selected exchange, and target-side JSON-RPC errors.
