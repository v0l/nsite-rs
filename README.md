# NSite-RS

A Nostr-based website hosting system that serves sites via subdomains.

## Features

- **NIP-5A Support**: Host websites on Nostr using kind 15128 (root sites) and kind 35128 (named sites)
- **Subdomain Routing**: 
  - Root sites: `npub1...` subdomains
  - Named sites: `<pubkeyB36><dTag>` subdomains (50-char base36 pubkey + d tag)
- **Directory Page**: Browse all published sites with profile information
- **Reactive UI**: Preact-based frontend with real-time profile updates
- **Caching**: 1-hour site info expiry with automatic refresh

## Tech Stack

- **Backend**: Rust with Axum
- **Frontend**: Preact with JSX (loaded from CDN, no build step)
- **Nostr**: @snort/system for relay interactions
- **Encoding**: @scure/base for base36 pubkey encoding

## Development

```bash
# Build
cargo build

# Run
cargo run -- --relay wss://relay.damus.io

# Format
cargo fmt

# Lint
cargo clippy
```

## Usage

1. Create a kind 15128 event with `/index.html` path tag for root sites
2. Create a kind 35128 event with `/index.html` path tag and `d` tag for named sites
3. Access sites via their respective subdomains

## License

MIT
