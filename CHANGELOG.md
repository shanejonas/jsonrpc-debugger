# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2024-01-XX

### Added
- Initial release of JSON-RPC Debugger
- Real-time monitoring of JSON-RPC requests and responses
- Request interception with pause/resume functionality
- External editor support for request/response modification
- Syntax highlighting for JSON content with proper indentation
- HTTP headers display for debugging transport details
- Vim-style navigation with comprehensive keyboard shortcuts
- Dynamic configuration (change target URL and port on the fly)
- Table view with status, transport, method, ID, and duration columns
- Custom response creation for intercepted requests
- Charles Proxy-style debugging workflow
- Command-line interface with port and target options
- Comprehensive test suite with 16+ tests

### Features
- **Interception modes**: Normal, Paused, Intercepting
- **External editor integration**: Uses $EDITOR, $VISUAL, or falls back to vim/nano/vi
- **Request modification**: Edit request body, headers, or create custom responses
- **Real-time updates**: Live display of request/response timing and status
- **Keyboard shortcuts**: Full vim-style navigation (j/k/d/u/G/g) plus arrow keys
- **Visual indicators**: Status icons (✓ Success, ✗ Error, ⏳ Pending, ⏸ Intercepted, ✏ Modified)
- **Scrolling support**: Both main details and intercept details panels support scrolling
- **JSON formatting**: 2-space indentation with syntax highlighting

### Technical
- Built with Rust and ratatui for terminal UI
- Uses warp for HTTP proxy server
- Async/await architecture with tokio
- Thread-safe state management with Arc<Mutex<>>
- Comprehensive error handling and input sanitization 