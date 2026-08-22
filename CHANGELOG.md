# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1] - 2026-08-21

### Changed

- `Enter` opens the selected response from Requests. `Ctrl-B y` copies any focused panel as Markdown.

### Fixed

- Request lists explain when a filter hides every row, and session changes clear stale filters.

## [0.3.0] - 2026-08-21

### Added

- Durable SQLite sessions in `~/.config/jsonrpc-debugger/sqlite.db`, with a session picker and paged history over the agent API.
- `debugger.listSessions`, `debugger.createSession`, and `debugger.selectSession` control methods.
- A `Ctrl-B` command prefix and `Ctrl-B ?` keybind help.
- Persistent line annotations with per-ID deletion over the agent API.
- Amber scrollbar markers and `Ctrl-B a` annotation prompts for visual selections.
- Inline notes for single lines and diagnostic rows for multiline ranges.
- Named session prompts, `Ctrl-B R` rename, and `debugger.renameSession`.
- Focused-panel fullscreen with `Ctrl-B z` and agent control.

### Changed

- Global TUI commands now live behind `Ctrl-B`. Actions for a focused intercepted request remain direct.
- Line highlights are temporary references. Annotations persist independently in session history.
- Request filters match method names and IDs.

## [0.2.0] - 2026-08-20

### Added

- A loopback JSON-RPC control plane with an OpenRPC document, agent-driven requests, interception controls, shared line references, and revision-based change waiting.
- Visible request and response line numbers. Click a line to reference it, then Shift-click to extend the range.
- Portable session export and replay. Replayed history never sends traffic to the target.
- Mouse focus, panel scrolling, clickable tabs and inputs, and Markdown clipboard output.
- An inline modal editor with Vim word motions, operators, paste, and undo.

### Changed

- New requests run in the background instead of blocking or leaving the TUI.
- CI tests every change. Tagged releases publish only when the tag matches the package version.

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
