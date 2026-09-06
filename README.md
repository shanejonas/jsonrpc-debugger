# JSON-RPC Debugger

Debug JSON-RPC yourself or hand the debugger to an agent.

`jsonrpc-debugger` is a local proxy and transparent stdio wrapper. Its terminal UI and localhost JSON-RPC control plane share the same live, durable session.

## Install

```bash
cargo install jsonrpc-debugger
```

Install the latest source instead:

```bash
cargo install --git https://github.com/shanejonas/jsonrpc-debugger
```

## HTTP proxy

```bash
jsonrpc-debugger --port 8080 --target http://localhost:8090
```

This starts the proxy on `http://127.0.0.1:8080` and the agent control plane on `http://127.0.0.1:8081`. Send JSON-RPC traffic to the proxy:

```bash
curl http://127.0.0.1:8080 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"example_ping","params":[]}'
```

Override the control plane with `--control-port`.

## Stdio servers

Use `wrap` when a real MCP, ACP, LSP, or DAP client should own the connection while the debugger observes and intercepts it:

```bash
# MCP and ACP: newline-delimited JSON
jsonrpc-debugger --control-port 8096 \
  wrap -- npx -y @modelcontextprotocol/server-everything

# LSP and DAP: Content-Length framing
jsonrpc-debugger --control-port 8096 \
  wrap --framing content-length -- gopls
```

Point the real client at the wrapper command, then open the TUI from another terminal:

```bash
jsonrpc-debugger attach http://127.0.0.1:8096
```

Use `stdio` when a person, curl, or an agent should drive the child through the debugger's HTTP proxy:

```bash
jsonrpc-debugger --port 8080 stdio -- npx my-mcp-server
jsonrpc-debugger --port 8080 stdio --framing content-length -- rust-analyzer
```

Stdio requests time out after 120 seconds. Change the limit with `--request-timeout <SECONDS>`.

## Terminal UI

The TUI combines request history, request and response details, interception, inline editing, search, and durable annotations. Press `Ctrl-B ?` for commands and keybindings.

Annotations appear as a flat chronological list beneath their source lines, including existing replies. Headers show author and age. Click a note or use `[` / `]` to select it, then `e` to edit or `Ctrl-B d` to delete it. Use visual selection and `Ctrl-B a` to add a note. These controls also work in attached TUIs. Existing reply relationships remain stored for API compatibility.

Sessions survive restarts in `~/.config/jsonrpc-debugger/sqlite.db`. Set `XDG_CONFIG_HOME` or `JSONRPC_DEBUGGER_CONFIG_DIR` to move the database.

## Agent control

The control plane is a JSON-RPC 2.0 server. Agents can inspect history, search saved sessions, annotate evidence, reveal matches in the TUI, send requests, and control interception while you watch.

Print the bundled agent skill:

```bash
jsonrpc-debugger --skill
```

Discover the live API:

```bash
jsonrpc-debugger call --transport=http http://127.0.0.1:8081 \
  '{"jsonrpc":"2.0","id":1,"method":"rpc.discover","params":{}}'
```

The complete API lives in [`openrpc.json`](openrpc.json).

`debugger.getUpdates` returns an initial snapshot, then only new exchanges and responses that complete older requests. Pass the previous session ID, `nextIndex`, and outstanding `pendingIndices` on subsequent calls. Attach uses this method, so update the wrapper and attached TUI together.

## Develop

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo run -- --target http://localhost:8090
```

Measure queued response pairing and large-payload rendering without adding benchmark dependencies:

```bash
cargo run --release --example performance --offline
```

Rust library callers read captured history through `App::exchanges()`. Use `add_message`, `append_exchanges`, or `activate_session` to change it while keeping request correlation consistent.

## License

MIT
