# NERC Keylogger

This repository contains a simple keylogger + server implementation built in Rust and HTML/JS, designed for demonstration, educational, or challenge usage (e.g. ICPC-style).

## Project Structure

```
nerc-keylogger/
├── keylog/                # client-side (agent) or “logger” component
│   └── …
├── keylogger-server/      # server-side component
│   └── …
├── Cargo.toml
├── .gitignore
└── README.md
```

- The **keylog** component runs on a client to capture keystrokes and send them (via HTTP) to the server.
- The **keylogger-server** collects incoming data, stores it, and serves a web interface to view logs.
- The **public/** folder hosts a minimal UI for viewing logs, interacting, or downloading data.

## Features

- Client-side key event capture and streaming to server
- Server-side collection, storage, and serving of logs
- Web UI to browse, search, download logs
- Simple, modular architecture (client / server in one repo)

## Quick Start

### Prerequisites

- Rust (1.60+ recommended)
- `cargo` toolchain installed

### Running locally

1. Clone the repo:
   ```bash
   git clone https://github.com/Mond1c/nerc-keylogger.git
   cd nerc-keylogger
   ```

2. Build and run server:
   ```bash
   cd keylogger-server
   cargo run --release
   ```
   This will launch the server (e.g. on `127.0.0.1:8080` or configured port).

3. Start the client logger:
   ```bash
   cd keylog
   cargo run --release
   ```
   Configure it to point to the server endpoint.

4. Open browser → `http://localhost:8080` (or appropriate address) to view logged keystrokes via the UI in `public/`.

## Configuration

The server can be configured via a `config.toml` (or environment) file:

```toml
bind_addr = "127.0.0.1:8080"
upload_dir = "uploads"
```

You can override via environment variables (e.g. `BIND_ADDR`, `UPLOAD_DIR`).

## API / Protocol

- **HTTP / REST** endpoints for upload, list, delete, serve files
- Web UI interacts via JSON (e.g. `GET /api/list`, `POST /api/upload`)
- Sanitize filenames, atomic writes (temp + rename) to avoid corruption

## Security Considerations

This project is for educational or contest use. If adapting to a real-world scenario:

- **Sanitize all inputs** (filenames, paths) to avoid directory traversal
- **Rate-limit and size-limit** uploads to prevent DoS
- Use **TLS / HTTPS** for network security
- Add **authentication & authorization**
- Consider **logging, error handling, audit trails**


