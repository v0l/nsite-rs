# NIP-5A Site Format Guidelines

## Site Types

| Type | Kind | Subdomain Format |
|------|------|------------------|
| Root Site | 15128 | `npub1...` (bech32) |
| Named Site | 35128 | `<pubkeyB36><dTag>` |

## Subdomain Encoding

### Root Sites (kind 15128)
- Accessed via `npub1...` bech32-encoded public key
- Example: `http://npub1.../`

### Named Sites (kind 35128)
- Accessed via `<pubkeyB36><dTag>` subdomain
- **pubkeyB36**: 32-byte pubkey encoded with base36 (lowercase, 0-9 then a-z), exactly 50 characters
- **dTag**: The `d` tag value from the event
- Example: `http://<50-char-base36-pubkey>dtag/`

## Event Requirements

All site events must have:
- **Kind**: 15128 (root) or 35128 (named)
- **Path Tag**: `path:/index.html`
- **d Tag**: For named sites only, `d:<name>`

Optional tags:
- **source**: `source:<url>` links to site source code

## Frontend Implementation

The directory page (`src/index.html`) uses:
- Preact from CDN (no build step)
- `@scure/base` for base36 encoding
- Custom base36 encoder using BigInt (since `@scure/base` doesn't export base36)

Encoding example:
```javascript
const pubkeyBytes = hex.decode(event.pubkey);
const pubkeyB36 = base36.encode(pubkeyBytes).toLowerCase();
```
