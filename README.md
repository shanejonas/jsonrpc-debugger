# JSON-RPC Debugger

Debug JSON-RPC yourself or hand the debugger to an agent.

`jsonrpc-debugger` is a local JSON-RPC proxy with two interfaces over the same live session:

- A terminal UI for people.
- A localhost JSON-RPC control plane for agents and scripts.

Both can inspect history, send requests, intercept traffic, focus panels, scroll, and point at exact request or response lines.

## Install

```bash
cargo install jsonrpc-debugger
```

Install the latest source instead:

```bash
cargo install --git https://github.com/shanejonas/jsonrpc-debugger
```

## Start

```bash
jsonrpc-debugger --port 8080 --target http://localhost:8090
```

This starts:

- The JSON-RPC proxy on `http://127.0.0.1:8080`.
- The agent control plane on `http://127.0.0.1:8081`.

Send traffic to the proxy:

```bash
curl http://127.0.0.1:8080 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'
```

The control port defaults to the proxy port plus one. Override it with `--control-port`.

## Use it yourself

The TUI shows request history beside the selected request and response. It supports the keyboard, mouse, and an inline Vim-style JSON editor.

| Action | Input |
| --- | --- |
| Focus a panel | Hover or click it |
| Scroll a panel | Mouse wheel or `j` / `k` |
| Change tabs or inputs | Click them |
| Select a line | Click its line number |
| Select a line range | Click, then Shift-click |
| Copy the focused panel as Markdown | `Enter` |
| Pause new requests | `p` |
| Allow or block an intercepted request | `a` / `b` |
| Edit a request body or headers | `e` / `h` |
| Create a request or custom response | `c` |
| Quit | `q` |

The request list copies as a Markdown table. Request bodies, responses, headers, and status copy as Markdown.

The inline editor supports normal Vim motions and operators such as `w`, `b`, `e`, `cw`, `dw`, `dd`, `u`, and `p`. Save with `:w`; cancel with `:q!`.

## Let an agent drive it

The control plane is itself a JSON-RPC 2.0 server. An agent can operate the debugger while you watch the same actions happen in the TUI.

Ask the running debugger what it supports:

```bash
curl http://127.0.0.1:8081 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"rpc.discover"}'
```

Read its current state:

```bash
curl http://127.0.0.1:8081 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":2,"method":"debugger.getState"}'
```

An agent can:

- Read state, history, pending requests, and numbered panel content.
- Wait for revisions without polling.
- Send requests through the debugger.
- Select exchanges, focus panels, scroll, and highlight line ranges.
- Change the target or filter and control interception.
- Export a session or replay one without forwarding its requests.

Line selections are shared. Click a suspicious response line, tell the agent to inspect the selected line, and it reads the same reference from `debugger.getState`. The agent can reveal another line or range and bring it into view for you.

Use the [`jsonrpc-debugger` agent skill](https://github.com/shanejonas/agent-skills/tree/main/skills/jsonrpc-debugger) for typed call and demo scripts. The complete API lives in [`openrpc.json`](openrpc.json) and is available at runtime through `rpc.discover`.

## Intercept requests

Press `p` or call `debugger.setPaused`. New requests wait in the debugger until a person or agent allows, blocks, edits, or completes them with a custom response.

## Develop

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo run -- --target http://localhost:8090
```

## License

MIT
