# NSite-RS: NIP-5A Gateway

A high-performance Rust gateway that serves decentralized websites published as Nostr events, implementing [NIP-5A](https://github.com/nostr-protocol/nips/blob/master/5A.md) (Content Hosting).

## What is NSite?

NSite transforms your Nostr relays into a decentralized web hosting platform. Websites are published as signed Nostr events and served via subdomains, enabling:

- **Censorship-resistant hosting** - Content is distributed across Nostr relays
- **No infrastructure** - Sites are served directly from Nostr events
- **Built-in identity** - Public keys serve as domain ownership proof
- **Zero configuration** - Subdomain routing handles site discovery automatically

## Quick Start

```bash
cargo run -- --relay wss://relay.damus.io
```

The gateway will:
1. Connect to specified Nostr relays
2. Listen for site events (kinds 15128, 35128)
3. Serve sites via subdomain routing
4. Display a directory at the root domain

## Site Types

| Type | Kind | Subdomain Format | Use Case |
|------|------|------------------|----------|
| **Root Site** | 15128 | `npub1...` | Personal sites, blogs |
| **Named Site** | 35128 | `<pubkeyB36><dTag>` | Projects, organizations |

### Named Site Example
- Pubkey (hex): `9ec7a778167afb1d30c4833de9322da0c08ba71a69e1911d5578d3144bb56437`
- d tag: `aa`
- Subdomain: `3ygtacgoaw2nnorgkiatssdw30bzgikmmveotztz116wa7fkfraa aa`
- URL: `http://3ygtacgoaw2nnorgkiatssdw30bzgikmmveotztz116wa7fkfraa aa/`

## Publishing a Site

Create a Nostr event with:

```json
{
  "kind": 35128,
  "tags": [
    ["d", "myproject"],
    ["path", "/index.html"],
    ["source", "https://github.com/..."]
  ],
  "content": "..."
}
```

### Required Tags
- **`d`** - Unique identifier (for named sites)
- **`path`** - File path mapping (e.g., `/index.html`, `/style.css`)

### Optional Tags
- **`source`** - Link to source code/repository

## Architecture

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│   Browser    │────▶│  Axum Router │────▶│ Site Handler │
└──────────────┘     └──────────────┘     └──────────────┘
                    (subdomain-based)           │
                                                ▼
                                       ┌──────────────┐
                                       │ Nostr Relay  │
                                       │ (manifest +  │
                                       │  content)    │
                                       └──────────────┘
```

### Request Flow
1. Browser requests `http://<subdomain>/path/to/file`
2. Axum extracts pubkey from subdomain
3. Site handler fetches manifest (kind 15128/35128) from relays
4. Manifest maps paths to content hashes
5. Content fetched from Blossom servers or relay attachments
6. Response cached for subsequent requests

## Features

- **Concurrent loading** - Multiple assets loaded in parallel
- **Transparent caching** - Content cached to temp directory
- **Multiple relay support** - Fallback across relays for resilience
- **Profile integration** - Displays author avatars and names from Nostr metadata
- **Directory page** - Auto-generated site listing at root domain

## Development

```bash
# Build
cargo build

# Release build
cargo build --release

# Check formatting
cargo fmt --check

# Lint
cargo clippy -- -D warnings

# Run with custom relay
cargo run -- --relay wss://relay.damus.io --relay wss://nos.lol
```

## Dependencies

- **Rust**: Axum, tokio, reqwest, nostr-sdk
- **Frontend**: Preact, @snort/system, @scure/base
- **TLS**: rustls with aws-lc-rs

## NIP-5A Compliance

This gateway fully implements NIP-5A:
- ✅ Root site events (kind 15128)
- ✅ Named site events (kind 35128)
- ✅ Subdomain routing based on pubkey encoding
- ✅ Path tag resolution
- ✅ d tag validation for named sites

## Live Demo

Visit https://nwb.tf to see the directory in action.

## License

MIT
