# JSON-RPC Debugger

A terminal-based JSON-RPC debugger with interception capabilities, built with Rust and ratatui. Inspect, modify, and debug JSON-RPC requests and responses in real-time.

Demo video of pointing metamask JSON-RPC at the debugger:

https://github.com/user-attachments/assets/20a23f55-e3b8-44b1-9536-fcc1fd6e09dc




## Features

- 🔍 **Real-time monitoring** of JSON-RPC requests and responses with timing information
- ⏸️ **Request interception** - pause, inspect, and modify requests before forwarding
- 🎨 **Syntax highlighting** for JSON content with proper indentation
- 📊 **HTTP headers display** for debugging transport details
- ⌨️ **Vim-style navigation** with comprehensive keyboard shortcuts
- 🎯 **Dynamic configuration** - change target URL and port on the fly
- 📝 **Inline modal editor** with core Vim motions and operators
- 📋 **Table view** with status, transport, method, ID, and duration columns
- 🔄 **Custom response creation** for intercepted requests
- 🖱️ **Mouse support** for hover focus, scrolling, tabs, inputs, and line references
- 🤖 **JSON-RPC control plane** for agents and scripts

## Installation

### Prerequisites

- Rust 1.70+ (install from [rustup.rs](https://rustup.rs/))

### Install from crates.io

```bash
cargo install jsonrpc-debugger
```

### Install from GitHub

```bash
cargo install --git https://github.com/shanejonas/jsonrpc-debugger
```

### Build from source

```bash
git clone https://github.com/shanejonas/jsonrpc-debugger.git
cd jsonrpc-debugger
cargo build --release
```

### Install locally

```bash
cargo install --path .
```

## Usage

### Basic Usage

Start the debugger with default settings (port 8080, no default target):

```bash
jsonrpc-debugger
# or during development:
cargo run
```

### Command Line Options

```bash
# Custom port
jsonrpc-debugger --port 9090

# Custom target URL
jsonrpc-debugger --target https://your-api.com

# Both custom port and target
jsonrpc-debugger --port 9090 --target https://your-api.com

# Override the control plane port (defaults to proxy port + 1)
jsonrpc-debugger --port 9090 --control-port 9999

# Show help
jsonrpc-debugger --help
```

### Making Requests

Once the debugger is running, send JSON-RPC requests to the proxy:

```bash
curl -X POST http://localhost:8080 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"your_method","params":[],"id":1}'
```

## Interface Overview

The TUI is divided into three main sections:

```
┌─ Status ─────────────────────────────────────────────────────────────────────────────┐
│ JSON-RPC Debugger | Status: RUNNING | Port: 8080 | Target: https://api.example.com   │
└──────────────────────────────────────────────────────────────────────────────────────┘
┌─ JSON-RPC ────────────────────────────┐ ┌─ Details ──────────────────────────────────┐
│ Status    │Transport│Method    │ID│Dur│ │ Transport: Http                            │
│ ✓ Success │ HTTP    │ eth_call │1 │45m│ │ Method: eth_call                           │
│ ✗ Error   │ HTTP    │ eth_send │2 │12m│ │ ID: 1                                      │
│⏳ Pending │ HTTP    │ eth_block│3 │11m│ │                                            │
│                                       │ │ REQUEST:                                   │
│                                       │ │ HTTP Headers:                              │
│                                       │ │   content-type: application/json           │
│                                       │ │                                            │
│                                       │ │ JSON-RPC Request:                          │
│                                       │ │ {                                          │
│                                       │ │   "jsonrpc": "2.0",                        │
│                                       │ │   "method": "eth_call",                    │
│                                       │ │   "params": [...],                         │
│                                       │ │   "id": 1                                  │
│                                       │ │ }                                          │
└───────────────────────────────────────┘ └────────────────────────────────────────────┘
┌─ Controls ───────────────────────────────────────────────────────────────────────────┐
│ q quit | ↑↓/^n/^p navigate | j/k/d/u/G/g scroll | s start/stop | t target | p pause  │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

### Status Indicators

- ✓ **Success** - Request completed successfully
- ✗ **Error** - Request returned an error
- ⏳ **Pending** - Request sent, waiting for response

## Keyboard Shortcuts

### Navigation
- `↑/↓` or `Ctrl+p/Ctrl+n` - Navigate between requests
- `j/k` - Scroll details panel up/down (vim-style)
- `d/u` or `Ctrl+d/Ctrl+u` - Page down/up in details
- `G` - Go to bottom of details
- `g` - Go to top of details
- `Enter` - Copy the focused panel as Markdown
- Move the mouse over a panel to focus it
- Use the mouse wheel to scroll the panel under the pointer
- Click a request or response line to focus and highlight it
- Shift-click another line to extend the highlighted range
- Click tabs, target, filter, or status controls to use them

### Proxy Control
- `s` - Start/stop the proxy server
- `t` - Edit target URL
- `c` - Create new request (normal mode) / Complete request with custom response (intercept mode)
- `q` - Quit application

### Interception Mode
- `p` - Toggle pause mode (intercept new requests)
- `a` - Allow selected intercepted request
- `e` - Edit selected request body
- `h` - Edit selected request headers
- `c` - Complete request with custom response
- `b` - Block selected request
- `r` - Resume all pending requests

## Request Interception

The debugger supports Charles Proxy-style request interception:

1. **Enable pause mode**: Press `p` to start intercepting requests
2. **Make requests**: Send JSON-RPC requests to the proxy
3. **Inspect**: Intercepted requests appear in the pending list with ⏸ icon
4. **Modify**: 
   - Press `e` to edit request body
   - Press `h` to edit HTTP headers
   - Press `c` to create a custom response
5. **Control**: Press `a` to allow, `b` to block, or `r` to resume all

### Inline Editor

The editor stays inside the TUI:

- `i`, `a`, `I`, `A`, `o`, or `O` - Enter insert mode
- `h`, `j`, `k`, `l`, `w`, `b`, `e`, `0`, `$`, `gg`, or `G` - Move
- `d`, `c`, or `y` plus a motion - Delete, change, or yank (`dw`, `cw`, `db`, `de`, and so on)
- `dd`, `cc`, or `yy` - Delete, change, or yank a line
- `x`, `X`, `D`, `C`, `s`, or `S` - Edit characters or the current line
- `p` or `P` - Paste after or before
- `u` - Undo
- `Esc` - Return to normal mode
- `:w`, `:wq`, `:x`, or `Ctrl+S` - Save
- `:q`, `:q!`, or `q` in normal mode - Cancel

Modified requests show with a ✏ icon and [MODIFIED] or [BODY]/[HEADERS] labels.

## Configuration

### Port Conflicts

Some ports may conflict with system services:
- **Port 7000**: Used by Apple AirPlay on macOS
- **Port 5000**: Often used by other development tools

Use alternative ports like 8080, 9090, 3000, 4000, 8000, or 8888.

## Examples

### Basic Monitoring

```bash
# Start debugger
jsonrpc-debugger --port 8080

# Set target URL in the TUI (press 't')
# Then make requests in another terminal
curl -X POST http://localhost:8080 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'
```

### Request Interception

1. Start the debugger: `jsonrpc-debugger`
2. Set target URL: Press `t` and enter your target
3. Enable pause mode: Press `p`
4. Make a request (it will be intercepted)
5. Edit the request: Press `e` to modify body or `h` for headers
6. Allow the modified request: Press `a`

### Custom Responses

1. Enable pause mode and intercept a request
2. Press `c` to create a custom response
3. Edit the JSON response in the inline editor
4. The custom response is sent back to the client

### Creating New Requests

1. Press `c` in normal mode
2. Edit the JSON-RPC request template in the inline editor
3. Save it. The request runs in the background while the TUI stays responsive

## Agent Control Plane

The debugger exposes a JSON-RPC 2.0 control plane on loopback. Its port defaults to the proxy port plus one, so a proxy on `8080` has a control plane on `8081`.

Ask it for its OpenRPC document:

```bash
curl http://127.0.0.1:8081 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"rpc.discover"}'
```

An agent can read state and history, wait for changes, send requests through the debugger, select an exchange, focus or scroll a TUI panel, and highlight numbered request or response lines. A user click creates the same line reference, exposed as `lineSelection` by `debugger.getState`, so the user and agent can discuss the same evidence.

Read numbered panel lines, then reveal one:

```bash
curl http://127.0.0.1:8081 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"debugger.getPanel","params":{"panel":"response"}}'

curl http://127.0.0.1:8081 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":2,"method":"debugger.revealLines","params":{"panel":"response","startLine":8}}'
```

The control plane can also set the target or filter, pause interception, inspect pending requests, and allow, block, or complete them. Every call updates the runtime state shown in the TUI.

Wait without polling. Pass the latest `revision` from `debugger.getState`; the call returns when state changes or the timeout expires:

```bash
curl http://127.0.0.1:8081 \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":3,"method":"debugger.waitForChange","params":{"afterRevision":12,"timeoutMs":30000}}'
```

`debugger.exportSession` returns portable history JSON. Pass that value to `debugger.replaySession` to append it to the visible history. Replay never forwards requests to the target.

Send a request through the debugger:

```bash
curl http://127.0.0.1:8081 \
  -H 'content-type: application/json' \
  -d '{
    "jsonrpc":"2.0",
    "id":1,
    "method":"debugger.sendRequest",
    "params":{"request":{"jsonrpc":"2.0","id":42,"method":"eth_blockNumber","params":[]}}
  }'
```

## Troubleshooting

### Port Already in Use

If you get a "port already in use" error:
```bash
# Check what's using the port
netstat -an | grep :8080

# Use a different port
jsonrpc-debugger --port 9090
```

### Connection Refused

If requests fail with "connection refused":
- Check that the target URL is correct and reachable
- Verify the target server is running
- Test the target directly with curl
- Make sure you've set a target URL (press `t` in the TUI)

### Request Errors

New requests run in the background. Connection, HTTP, and JSON errors appear at the bottom of the TUI.

### JSON Formatting Issues

The debugger displays JSON with:
- 2-space indentation
- Syntax highlighting (keys in cyan, strings in green, numbers in blue, etc.)
- Proper line breaks and formatting

If JSON appears malformed, check that the original request/response is valid JSON.

## Development

### Running Tests

```bash
cargo test
```

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release
```

### Project Structure

```
src/
├── main.rs          # CLI and main application loop
├── app.rs           # Application state and logic
├── ui.rs            # TUI rendering and layout
├── proxy.rs         # HTTP proxy server implementation
└── lib.rs           # Library exports for testing
```

## License

MIT License - see LICENSE file for details.

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests if applicable
5. Submit a pull request

## Acknowledgments

- Built with [ratatui](https://github.com/ratatui-org/ratatui) for the terminal UI
- Uses [warp](https://github.com/seanmonstar/warp) for the HTTP proxy server
- Inspired by Charles Proxy and similar debugging tools 
