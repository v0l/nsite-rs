# NSite-RS Agent Guidelines

## Build, Lint, and Test Commands

### Build
```bash
cargo build              # Debug build
cargo build --release    # Release build
cargo check              # Quick syntax/type check
```

### Run
```bash
cargo run -- --relay wss://relay.damus.io
```

### Tests
No tests currently exist. To run tests:
```bash
cargo test               # Run all tests
cargo test -- --nocapture  # Run tests with output
cargo test test_name     # Run a single test by name
```

### Lint/Format
```bash
cargo fmt                # Format code
cargo fmt --check        # Check formatting (fails if unformatted)
cargo clippy             # Run lint checks
cargo clippy -- -D warnings  # Treat warnings as errors
```

---

## Code Style Guidelines

### Imports
- Use `use crate::module::Item` for internal modules
- Use `use external_crate::Item` for external crates
- Group imports: std/crate-specific imports together, then external crates
- Avoid wildcard imports (`use X::*`) except `nostr_sdk::prelude::*`
- Place `mod module_name;` at the top of files, before imports

### Formatting
- 4-space indentation (Rust default)
- Max line length: 120 characters
- Use `cargo fmt` to auto-format
- Wrap function arguments when exceeding line limit
- Put opening braces on same line as declaration

### Types
- Use explicit types for function parameters and return values
- Infer types locally where clear (`let x = value;`)
- Prefer strong typing over `String`/`bool` for domain concepts
- Use `Result<T, E>` for fallible operations, `Option<T>` for nullable
- Use `anyhow::Result` for application errors, `bail!()` for early returns
- Type aliases for complex types: `type SiteMap = Arc<RwLock<HashMap<...>>>;`

### Naming Conventions
- **Functions/Methods**: `snake_case` (e.g., `load_route`, `fetch_manifest`)
- **Types/Structs**: `PascalCase` (e.g., `SiteInfo`, `SiteRoute`)
- **Constants**: `SCREAMING_SNAKE_CASE` (e.g., `DEFAULT_TIMEOUT`)
- **Variables**: `snake_case` (e.g., `site_map`, `pubkey`)
- **Modules**: `snake_case` (e.g., `site.rs`, `main.rs`)
- **Private fields**: Prefix with underscore if unused (`_temp`)

### Error Handling
- Use `anyhow::Result` for application-level errors
- Use `bail!("message")` for early returns with errors
- Use `?` operator to propagate errors
- Use `warn!`, `error!`, `info!`, `debug!` macros for logging
- Provide context in errors: `Err(anyhow!("failed to load route: {}", e))`
- Return `Option<T>` when absence is valid (not an error)

### Async Code
- Use `tokio::sync::RwLock` for shared async state
- Use `.await` after all async operations before using results
- Avoid blocking operations in async contexts
- Use `Arc<T>` for sharing data across tasks
- Prefer `read()` over `write()` when mutation not needed

### Documentation
- Use `///` doc comments for public APIs
- Use `/** */` for multi-line module-level docs
- Document complex logic with inline `//` comments
- Explain "why" not just "what" in comments
- Include examples in doc comments when helpful

### Axum Framework
- Use `#[tokio::main]` for async main function
- Use `Router::new().route("/", get(handler))` for routing
- Use `State<T>` extractor for managed application state
- Use `axum::extract::Request` for custom header access
- Use `axum::serve(listener, app)` for server startup
- Return `Result<Response, StatusCode>` or `impl IntoResponse` from handlers

### Nostr Integration
- Use `nostr_sdk::Client` for relay operations
- Use `Filter` for querying events
- Use `Kind::Custom(n)` for NIP-defined event kinds
- Handle bech32 encoding with `Nip19::from_bech32()`
- Use `PublicKey::from_slice()` for key operations

### Concurrency
- Protect shared state with `RwLock`
- Minimize lock scope: drop locks before long operations
- Use `clone()` to transfer ownership before async blocks
- Avoid deadlocks: acquire locks in consistent order

### File I/O
- Use `tokio::fs` for async file operations
- Use `temp_dir()` for temporary files
- Create directories before writing: `create_dir_all()`
- Handle file not found gracefully with `Option`

### HTTP/Networking
- Use `reqwest` for HTTP client operations
- Check status codes: `r.status().is_success()`
- Handle connection errors with `warn!` or `error!`
- Set appropriate timeouts for network requests

### Security
- Validate all external input (pubkeys, paths, URLs)
- Never log secrets or private keys
- Sanitize file paths to prevent directory traversal
- Use HTTPS for network requests when possible
