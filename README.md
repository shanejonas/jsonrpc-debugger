# JSON-RPC Debugger

A terminal-based JSON-RPC debugger with interception capabilities, built with Rust and ratatui. Inspect, modify, and debug JSON-RPC requests and responses in real-time.

## Features

- 🔍 **Real-time monitoring** of JSON-RPC requests and responses with timing information
- ⏸️ **Request interception** - pause, inspect, and modify requests before forwarding
- 🎨 **Syntax highlighting** for JSON content with proper indentation
- 📊 **HTTP headers display** for debugging transport details
- ⌨️ **Vim-style navigation** with comprehensive keyboard shortcuts
- 🎯 **Dynamic configuration** - change target URL and port on the fly
- 📝 **External editor support** for request/response modification
- 📋 **Table view** with status, transport, method, ID, and duration columns
- 🔄 **Custom response creation** for intercepted requests

## Installation

### Prerequisites

- Rust 1.70+ (install from [rustup.rs](https://rustup.rs/))

### Build from source

```bash
git clone https://github.com/your-username/jsonrpc-debugger.git
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
┌Status──────────────────────────────────────────────────────────────────────────────────┐
│JSON-RPC Debugger | Status: RUNNING | Port: 8080 | Target: https://api.example.com      │
└────────────────────────────────────────────────────────────────────────────────────────┘
┌JSON-RPC───────────────────────────────┐┌Details────────────────────────────────────┐
│Status    │Transport│Method     │ID │Dur││Transport: Http                            │
│✓ Success │HTTP     │eth_call   │1  │45ms││Method: eth_call                           │
│✗ Error   │HTTP     │eth_send   │2  │12ms││ID: 1                                      │
│⏳ Pending │HTTP     │eth_block  │3  │-   ││                                           │
│                                       ││REQUEST:                                   │
│                                       ││HTTP Headers:                              │
│                                       ││  content-type: application/json           │
│                                       ││                                           │
│                                       ││JSON-RPC Request:                          │
│                                       ││{                                          │
│                                       ││  "jsonrpc": "2.0",                        │
│                                       ││  "method": "eth_call",                    │
│                                       ││  "params": [...],                         │
│                                       ││  "id": 1                                  │
│                                       ││}                                          │
└───────────────────────────────────────┘└───────────────────────────────────────────┘
┌Controls────────────────────────────────────────────────────────────────────────────────┐
│q quit | ↑↓/^n/^p navigate | j/k/d/u/G/g scroll | s start/stop | t target | p pause     │
└────────────────────────────────────────────────────────────────────────────────────────┘
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

### Proxy Control
- `s` - Start/stop the proxy server
- `t` - Edit target URL
- `c` - Create new request (normal mode) / Complete request with custom response (intercept mode)
- `q` - Quit application

### Interception Mode
- `p` - Toggle pause mode (intercept new requests)
- `a` - Allow selected intercepted request
- `e` - Edit selected request body in external editor
- `h` - Edit selected request headers in external editor
- `c` - Complete request with custom response
- `b` - Block selected request
- `r` - Resume all pending requests

## Request Interception

The debugger supports Charles Proxy-style request interception:

1. **Enable pause mode**: Press `p` to start intercepting requests
2. **Make requests**: Send JSON-RPC requests to the proxy
3. **Inspect**: Intercepted requests appear in the pending list with ⏸ icon
4. **Modify**: 
   - Press `e` to edit request body in your external editor
   - Press `h` to edit HTTP headers
   - Press `c` to create a custom response
5. **Control**: Press `a` to allow, `b` to block, or `r` to resume all

### External Editor

The debugger uses your system's default editor for request modification:
- Checks `$EDITOR` environment variable
- Falls back to `$VISUAL`
- Defaults to `vim`, then `nano`, then `vi`

Modified requests show with a ✏ icon and [MODIFIED] or [BODY]/[HEADERS] labels.

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
3. Edit the JSON response in your external editor
4. The custom response is sent back to the client

### Creating New Requests

1. Press `c` in normal mode
2. Edit the JSON-RPC request template in your external editor
3. The request is sent through the proxy to the target

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

### Editor Not Found

If external editing fails:
```bash
# Set your preferred editor
export EDITOR=code  # VS Code
export EDITOR=nano  # Nano
export EDITOR=vim   # Vim
```

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