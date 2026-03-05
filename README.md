# Mail Man

A lightweight, offline-first API client for people who want a Postman-style workflow without cloud lock-in.

No account wall. No surprise workspace limits. No background sync you did not ask for.

Mail Man is built for developers who just want to send requests, manage environments securely, and keep full control of local data.

## Why This Exists

Many developers used Postman because it was quick and easy. Over time, pricing and free-tier limits changed for some workflows.

Mail Man is a simple alternative:
- Free and local-first
- Works offline
- Stores environment variables encrypted at rest
- Cross-platform (macOS, Windows, Linux)

## Core Features

- Request builder with common HTTP methods (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`, etc.)
- Body modes: `none`, `raw`, `urlencoded`, `form-data`, `binary`
- Environment variables with placeholder support (`${token}`, `${api_host}`)
- Postman placeholder compatibility (`{{token}}` is normalized on import)
- One-click cURL copy for selected request
- Response viewer with status, timing, headers, and body
- Encrypted environment files using Argon2id + XChaCha20-Poly1305
- Postman import from collection/environment exports, cache/leveldb, and requester logs (workspace-aware)
- Password-protected bundle export/import for moving workspaces across machines/OSes

## In One Line

Mail Man is the practical API workbench for developers who prefer local files over cloud dashboards.

## Notable Guarantees

- Offline-first app behavior (no account required)
- No built-in cloud sync
- Master password is not stored
- If master password is lost, encrypted environment values cannot be recovered

## Quick Start

### Prerequisites

- Rust toolchain (stable)
- `cargo`

### Run (Dev)

```bash
cargo run
```

### Build + Run (Release)

```bash
cargo build --release
./target/release/mailman
```

## Packaging Installers (Tauri Bundler Ecosystem)

This repo is pre-configured for `cargo-packager` via `Cargo.toml` (`[package.metadata.packager]`)
to produce:
- macOS: `.app` and `.dmg`
- Linux: `.deb`
- Windows: `.msi` and `.exe` (NSIS)

### 1. Install packager CLI

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
- A verifier is stored only to validate unlock attempts, not to recover passwords.

## Data Storage

Mail Man stores local app data in OS-appropriate user data directories (via `directories::ProjectDirs`).

Stored data includes:
- Request definitions
- Encrypted environment files
- App config and selection state
- Security metadata (salt + encrypted verifier)

## Postman Import

You can import from:
- Default Postman directories (auto mode)
- A custom path
- A specific workspace name filter (optional)

Mail Man attempts to merge imported data intelligently:
- Deduplicates requests using source metadata and request identity
- Merges missing headers/details into existing entries
- Merges environment variables without overwriting existing keys by default

## Project Goals

- Keep the app fast and small
- Preserve local ownership of API tooling data
- Stay dependency-pragmatic and maintainable
- Remain a practical daily driver for common API workflows

## Status

Active project. Core request workflow, encryption, and Postman migration paths are implemented.

## Contributing

Issues and PRs are welcome.

If you open an issue, include:
- OS and version
- Steps to reproduce
- Expected vs actual behavior
- Logs or screenshots if relevant

## FAQ

### Is this a Postman fork?

No. Mail Man is an independent project and is not affiliated with Postman.

### Does this replace every Postman feature?

No. It focuses on the core request-and-environment workflow with local security and migration support.

### Can I use this fully offline?

Yes for app behavior and data management. Network is only used when you send requests to your own APIs.
