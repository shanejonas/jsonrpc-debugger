# JSON-RPC Proxy TUI

A terminal-based JSON-RPC proxy with interception capabilities, built with Rust and ratatui. Inspect, modify, and debug JSON-RPC requests and responses in real-time.

## Features

- 🔍 **Real-time monitoring** of JSON-RPC requests and responses
- ⏸️ **Request interception** - pause, inspect, and modify requests before forwarding
- 🎨 **Syntax highlighting** for JSON content
- 📊 **HTTP headers display** for debugging
- ⌨️ **Vim-style navigation** with keyboard shortcuts
- 🎯 **Dynamic configuration** - change target URL and port on the fly
- 📝 **External editor support** for request modification

## Installation

### Prerequisites

- Rust 1.70+ (install from [rustup.rs](https://rustup.rs/))

### Build from source

```bash
git clone https://github.com/your-username/jsonrpc-proxy-tui.git
cd jsonrpc-proxy-tui
cargo build --release
```

## Usage

### Basic Usage

Start the proxy with default settings (port 8080, target: https://mock.open-rpc.org):

```bash
cargo run
```

### Command Line Options

```bash
# Custom port
cargo run -- --port 9090

# Custom target URL
cargo run -- --target https://your-api.com

# Both custom port and target
cargo run -- --port 9090 --target https://your-api.com

# Show help
cargo run -- --help
```

### Making Requests

Once the proxy is running, send JSON-RPC requests to the proxy:

```bash
curl -X POST http://localhost:8080 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"your_method","params":[],"id":1}'
```

## Interface Overview

The TUI is divided into three main sections:

```
┌Status──────────────────────────────────────────────────────────────────────────────────┐
│JSON-RPC Proxy TUI | Status: RUNNING | Port: 8080 | Target: https://mock.open-rpc.org   │
└────────────────────────────────────────────────────────────────────────────────────────┘
┌JSON-RPC Exchanges─────────────────────┐┌Exchange Details───────────────────────────┐
│→ [HTTP] foo (id: 1) ✓                 ││Transport: Http                            │
│→ [HTTP] bar (id: 2) ✗                 ││Method: foo                                │
│→ [HTTP] baz (id: 3) ⏳                ││ID: 1                                      │
│                                       ││                                           │
│                                       ││REQUEST:                                   │
│                                       ││HTTP Headers:                              │
│                                       ││  content-type: application/json          │
│                                       ││                                           │
│                                       ││JSON-RPC Request:                          │
│                                       ││{                                          │
│                                       ││  "jsonrpc": "2.0",                       │
│                                       ││  "method": "foo",                         │
│                                       ││  "params": [],                            │
│                                       ││  "id": 1                                  │
│                                       ││}                                          │
└───────────────────────────────────────┘└───────────────────────────────────────────┘
┌Controls────────────────────────────────────────────────────────────────────────────────┐
│q quit | ↑↓/^n/^p navigate | j/k/d/u/G/g scroll details | s start/stop | t edit target │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

### Status Indicators

- ✓ **Success** - Request completed successfully
- ✗ **Error** - Request returned an error
- ⏳ **Pending** - Request sent, waiting for response

## Keyboard Shortcuts

### Navigation
- `↑/↓` or `Ctrl+p/Ctrl+n` - Navigate between exchanges
- `j/k` - Scroll details panel up/down
- `d/u` - Page down/up in details
- `G` - Go to bottom of details
- `g` - Go to top of details

### Proxy Control
- `s` - Start/stop the proxy server
- `t` - Edit target URL
- `q` - Quit application

### Interception Mode
- `p` - Toggle pause mode (intercept new requests)
- `a` - Allow selected intercepted request
- `e` - Edit selected request body in external editor
- `h` - Edit selected request headers in external editor
- `c` - Complete selected request with custom response
- `b` - Block selected request
- `r` - Resume all pending requests

## Request Interception

The proxy supports Charles Proxy-style request interception:

1. **Enable pause mode**: Press `p` to start intercepting requests
2. **Make requests**: Send JSON-RPC requests to the proxy
3. **Inspect**: Intercepted requests appear in the pending list
4. **Modify**: Press `e` to edit request body or `h` to edit headers in your external editor
5. **Control**: Press `a` to allow, `c` to complete with custom response, `b` to block, or `r` to resume all

### External Editor

The proxy uses your system's default editor for request modification:
- Checks `$EDITOR` environment variable
- Falls back to `$VISUAL`
- Defaults to `vim`, then `nano`, then `vi`

### Custom Response Completion

The complete feature (`c` key) allows you to craft custom JSON-RPC responses without forwarding to the target server:

1. **Intercept a request**: Enable pause mode and make a request
2. **Press 'c'**: Opens your editor with a response template
3. **Edit response**: Modify the JSON-RPC response (must have `result` or `error`, not both)
4. **Save and exit**: The custom response is returned to the client

**Example response template:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": "custom response"
}
```

This is useful for:
- **Mocking responses** during development
- **Testing error conditions** by returning custom errors
- **API simulation** without a real backend

## Configuration

### Environment Variables

- `EDITOR` - Preferred text editor for request modification
- `VISUAL` - Alternative editor (fallback)

### Port Conflicts

Some ports may conflict with system services:
- **Port 7000**: Used by Apple AirPlay on macOS
- **Port 5000**: Often used by other development tools

Use alternative ports like 8080, 9090, 3000, 4000, 8000, or 8888.

## Examples

### Basic Monitoring

```bash
# Start proxy
cargo run -- --port 8080

# In another terminal, make requests
curl -X POST http://localhost:8080 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'
```

### Request Interception

1. Start the proxy: `cargo run`
2. Enable pause mode: Press `p`
3. Make a request (it will be intercepted)
4. Choose action:
   - Press `e` to edit request body
   - Press `h` to edit request headers  
   - Press `c` to complete with custom response
   - Press `a` to allow as-is
   - Press `b` to block

### Custom Target

```bash
# Proxy requests to your own JSON-RPC server
cargo run -- --target http://localhost:3000 --port 8080
```

## Troubleshooting

### Port Already in Use

If you get a "port already in use" error:
```bash
# Check what's using the port
netstat -an | grep :8080

# Use a different port
cargo run -- --port 9090
```

### Connection Refused

If requests fail with "connection refused":
- Check that the target URL is correct and reachable
- Verify the target server is running
- Test the target directly with curl

### Editor Not Found

If external editing fails:
```bash
# Set your preferred editor
export EDITOR=code  # VS Code
export EDITOR=nano  # Nano
export EDITOR=vim   # Vim
```

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