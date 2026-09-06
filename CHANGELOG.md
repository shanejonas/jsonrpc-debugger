# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0] - 2026-09-05

### Added

- `jsonrpc-debugger call --transport=http` sends a JSON-RPC request or batch and prints the complete response.
- Session search covers request and response headers, bodies, and annotations, with match navigation and scrollbar markers.
- `debugger.getUpdates` returns consistent incremental snapshots, including responses to older outstanding requests and resets when sessions change.
- A dependency-free performance example measures queued response pairing and large-payload rendering.

### Changed

- Annotations render as bordered cards, with scrolling and selection aligned to their source lines.
- Response pairing indexes pending requests and preserves newest-first matching for duplicate IDs.
- Detail panels format each payload once per draw, and attached TUIs redraw only after changes or input.
- Attach uses incremental updates and requires a wrapper with `debugger.getUpdates` support.
- Rust library history is read through `App::exchanges()`; the unused `get_details_content_lines` method is removed. `ControlClient::snapshot` takes `&App`, and `Snapshot::apply` returns the remote revision.
- History schema version 3 removes the redundant session/sequence index while retaining the unique constraint. Older binaries reject version 3 databases.
- Remove assignment-only tests, the assertion-free proxy constructor test, and trivial transport-name wrappers.

### Fixed

- Malformed upstream responses containing Unicode at preview truncation boundaries return JSON-RPC errors without panicking.

## [0.5.0] - 2026-08-30

### Added

- `debugger.find` searches durable sessions, exchanges, and annotations. Its line references open and highlight exact matches.
- Transparent stdio sessions can pause and resolve requests without taking ownership away from the real client.

### Changed

- The TUI uses a compact request sidebar with stacked details and stable diff-based redraws.
- Annotations use an inline editor and navigate across exchanges from any panel.
- Stdio requests have bounded waits so silent children cannot hang the debugger forever.

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
