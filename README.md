# NSite-RS

Nostr-based website hosting via subdomains.

## Quick Start

```bash
cargo run -- --relay wss://relay.damus.io
```

## How It Works

Sites are published as Nostr events and served via subdomains:

| Type | Kind | Subdomain Format |
|------|------|------------------|
| Root Site | 15128 | `npub1...` |
| Named Site | 35128 | `<50-char-base36-pubkey><dTag>` |

### Publishing a Site

Create a Nostr event with:
- **Kind**: 15128 (root) or 35128 (named)
- **Tags**: `path:/index.html`, `d:<name>` (for named sites), `source:<url>` (optional)
- **Content**: Your site data

### Accessing Sites

- Root: `http://npub1.../`
- Named: `http://<pubkeyB36><dTag>/`

## Directory

The root domain serves a Preact-powered directory page that lists all published sites with:
- Profile avatar and name
- Description (from profile)
- Creation date
- Source link (if provided)
- Visit button

## Architecture

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│   Browser    │────▶│  Axum Router │────▶│ Site Handler │
└──────────────┘     └──────────────┘     └──────────────┘
                              │                   │
                              ▼                   ▼
                      ┌──────────────┐     ┌──────────────┐
                      │ Root Domain  │     │ Subdomain    │
                      │ → Directory  │     │ → Site Files │
                      └──────────────┘     └──────────────┘
```

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
cargo run -- --relay wss://relay.damus.io
```

## Dependencies

- **Rust**: Axum, tokio, reqwest
- **Nostr**: nostr-sdk, @snort/system (frontend)
- **Frontend**: Preact, @scure/base

## License

MIT
