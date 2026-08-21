---
name: jsonrpc-debugger
description: Control a running jsonrpc-debugger TUI through its localhost JSON-RPC control plane. Use when an agent needs to inspect debugger state or durable history, send or intercept requests, drive visible focus and selections, annotate request or response lines, or demonstrate a JSON-RPC flow.
---

# JSON-RPC Debugger

Drive the live debugger the user sees. Treat the TUI as a shared screen: preserve unrelated state and leave it usable.

## Connect

1. Find the existing debugger process and its proxy port. The control port defaults to the proxy port plus one unless `--control-port` overrides it.
2. Probe the control endpoint with `rpc.discover`. Do not confuse it with the proxy port.
3. Read `debugger.getState` before changing anything. Record its session, target, filter, focus, selection, annotations, mode, pending count, and revision.

Drive the existing live process when its control endpoint responds. Do not start another debugger unless the user asks.

Use any JSON-RPC client. This shell helper is enough:

```bash
CONTROL_URL=http://127.0.0.1:8081
rpc() {
  local method="$1" params="${2-}"
  if [ -z "$params" ]; then params='{}'; fi
  curl -fsS "$CONTROL_URL" \
    -H 'content-type: application/json' \
    --data "$(printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"%s\",\"params\":%s}' "$method" "$params")"
}

rpc rpc.discover
rpc debugger.getState
rpc debugger.setFocus '{"panel":"history"}'
```

Treat a JSON-RPC `error` envelope as failure even when HTTP returns 200. The runtime OpenRPC document returned by `rpc.discover` is the authority for methods and parameters.

## Inspect Before Acting

- Use `debugger.getHistory` for recent traffic. Pass `sessionId` to inspect an older session without changing the TUI.
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

`debugger.annotateLines` does not select, focus, scroll, switch tabs, or highlight. Pass `exchangeIndex` and `tab` for background annotations. Messages must be one line and at most 160 characters. Remove only annotations you created, using their returned ID with `debugger.removeAnnotation`.

`debugger.sendRequest` sends a complete target JSON-RPC request through the proxy. Keep human-facing request IDs unique, semantic, and at most 12 characters.

## Run Dense Audits

1. Freeze the range and calculate the expected interval count before creating traffic.
2. Reuse a suitable durable session, or create one clearly named session when the user asks for a fresh run.
3. Send intervals oldest-to-newest, one at a time. Retry failures with backoff before advancing.
4. Inspect and annotate each completed response without selecting exchanges or changing focus, filter, scroll, tabs, or highlights.
5. Verify coverage, unique IDs, target JSON-RPC errors, and pending count.
6. Tell the user the audit is ready, rank the interesting findings, then reveal them one at a time during a guided walkthrough.

Do not visually select findings during the background audit. Persistent annotations and temporary highlights are separate tools.

## Intercept Requests

Interception requires concurrent calls:

1. Call `debugger.setPaused` with `paused: true`.
2. Start `debugger.sendRequest` without awaiting it.
3. Wait for a new revision, then read `debugger.getPending` until that request appears.
4. Resolve its internal pending `id` with `debugger.resolvePending` using `allow`, `block`, or `complete`.
5. Await the original send call.
6. Disable pause and verify the pending count returns to zero.

Use `allow` to forward the original or a replacement request, `block` for a debugger-generated error, and `complete` for a supplied response without forwarding.

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
