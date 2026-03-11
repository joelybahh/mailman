# Mailman

A lightweight, offline-first desktop API client for developers who want a Postman-style workflow without cloud lock-in.

No account wall. No forced sync. No surprise workspace limits.

Mailman is built for people who just want to send requests, manage environments securely, and keep full control of local data.

## Overview

- Local-first desktop API client
- Persistent request tabs with per-tab drafts
- Encrypted environments with a master password
- Optional OS-keychain-backed session persistence
- Postman import plus encrypted Mailman bundle export/import
- Cross-platform target: macOS, Windows, Linux

## Screenshots

UI screenshots and short workflow demos are coming soon.

## Why This Exists

Many developers used Postman because it was quick and easy. Over time, pricing and free-tier limits changed for some workflows.

Mailman is a simple alternative:
- Free and local-first
- Works offline
- Stores environment variables encrypted at rest
- Cross-platform (macOS, Windows, Linux)

## Core Features

- Persistent request tabs restored on launch
- Draft-based request editing, with unsaved changes retained per tab until you save
- Request builder with common HTTP methods (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`, etc.)
- Body modes: `none`, `raw`, `urlencoded`, `form-data`, `binary`
- Environment variables with placeholder support (`${token}`, `${api_host}`)
- Postman placeholder compatibility (`{{token}}` is normalized on import)
- Response scripts that extract JSON values from 2xx responses into environment variables
- Response viewer with status, timing, headers, raw body, and pretty JSON rendering
- One-click cURL copy for the active request
- Encrypted environment files using Argon2id + XChaCha20-Poly1305
- Optional "Keep me signed in" sessions backed by the OS keychain, plus a manual lock action
- Postman import from collection/environment exports, cache/LevelDB, and requester logs with optional workspace filtering
- Password-protected bundle export/import for moving or backing up workspaces across machines

## Notable Guarantees

- Offline-first desktop app behavior (no account required)
- No built-in cloud sync
- Master password itself is never stored
- If the master password is lost, encrypted environment values cannot be recovered

## Quick Start

### Prerequisites

- Rust toolchain (stable)
- `cargo`

### Run (Dev)

Mailman is a desktop GUI app, so the normal development entry point is:

```bash
cargo run
```

### Build + Run (Release)

```bash
cargo build --release
./target/release/mailman
```

## Packaging Installers (`cargo-packager`)

This repo is pre-configured for `cargo-packager` via `Cargo.toml` (`[package.metadata.packager]`) to produce:
- macOS: `.app` and `.dmg`
- Linux: `.deb`
- Windows: `.msi` and `.exe` (NSIS)

### 1. Install the packager CLI

```bash
cargo install cargo-packager --locked
```

### 2. Add your app icons

Place icons in `assets/icons/` with these filenames:
- `32x32.png`
- `128x128.png`
- `128x128@2x.png`
- `icon.icns`
- `icon.ico`

### 3. Build packages

```bash
make bundle-mac
make bundle-linux
make bundle-win
```

Or build every configured format:

```bash
make bundle-all
```

Artifacts are written to `dist/packager`.

## Security Model

- On first launch, you set a master password.
- Environment variable files are encrypted at rest.
- Key derivation uses Argon2id.
- Encryption uses XChaCha20-Poly1305.
- An encrypted verifier is stored only to validate unlock attempts, not to recover passwords.
- If session persistence is enabled, only the derived unlock key is cached in the OS keychain for the selected duration. The raw password is not stored.

## Data Storage

Mailman stores local app data in OS-appropriate user data directories (via `directories::ProjectDirs`).

Stored data includes:
- Saved request definitions
- Persisted request tabs and per-tab draft state
- Encrypted environment files
- App config, selection state, and workspace UI state
- Security metadata (salt + encrypted verifier)

## Postman Import

You can import from:
- Default Postman directories (auto mode)
- A custom path
- An optional workspace name filter

Mailman attempts to merge imported data intelligently:
- Deduplicates requests using source metadata and request identity
- Preserves collection and folder context where available
- Merges missing headers and request details into existing entries
- Merges environment variables without overwriting existing keys by default

## Contributing

Issues and PRs are welcome.

If you open an issue, include:
- OS and version
- Steps to reproduce
- Expected vs actual behavior
- Logs or screenshots if relevant
