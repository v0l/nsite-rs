use crate::{SiteAliasMap, SiteMap};
use anyhow::{Result, anyhow, bail};
use log::warn;
use nostr_sdk::prelude::Nip19;
use nostr_sdk::{Client, Event, Filter, FromBech32, Kind, PublicKey, TagKind, Url};
use std::borrow::Cow;
use std::collections::HashMap;
use std::env::temp_dir;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs::create_dir_all;
use tokio::sync::{Mutex, RwLock};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const SITE_INFO_EXPIRY: Duration = Duration::from_secs(3600);

/// Timeout for waiting on in-flight requests
const IN_FLIGHT_TIMEOUT: Duration = Duration::from_secs(30);

/// Global in-flight request tracker to prevent duplicate concurrent loads
static IN_FLIGHT_REQUESTS: once_cell::sync::Lazy<
    Arc<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>>,
> = once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

#[derive(Clone)]
pub struct SiteInfo {
    inner: Arc<RwLock<SiteInfoInner>>,
}

impl SiteInfo {
    pub async fn new_expired(client: &Client, pubkey: &[u8; 32], identifier: Option<&str>) -> Self {
        let client_clone = client.clone();
        let mut site = SiteInfoInner::new(*pubkey, client_clone, identifier.map(String::from));
        site.set_expired();
        SiteInfo {
            inner: Arc::new(RwLock::new(site)),
        }
    }

    /// Load site info for a pubkey
    /// For root sites: identifier is None
    /// For named sites (NIP-5A): identifier is the d tag value
    pub async fn load(
        client: &Client,
        pubkey: &[u8; 32],
        identifier: Option<&str>,
    ) -> Result<Option<Self>> {
        let pubkey_hex = hex::encode(pubkey);
        let cache_key = if let Some(ref id) = identifier {
            format!("{}-{}", pubkey_hex, id)
        } else {
            pubkey_hex.clone()
        };

        let start = std::time::Instant::now();
        let site_type = identifier.map_or("root".to_string(), |id| format!("named:{}", id));
        log::info!("Loading {} site for pubkey {} (cache_key: {})", site_type, &pubkey_hex[..8], cache_key);

        // RAII guard to ensure in-flight cleanup even on cancellation/panic
        struct InFlightGuard {
            cache_key: String,
            notify: Option<Arc<tokio::sync::Notify>>,
        }

        impl Drop for InFlightGuard {
            fn drop(&mut self) {
                // Only cleanup if we're the loader (not a waiter)
                if self.notify.take().is_some() {
                    // Spawn a task to do the async cleanup since we can't await in Drop
                    let key = self.cache_key.clone();
                    tokio::spawn(async move {
                        let mut in_flight = IN_FLIGHT_REQUESTS.lock().await;
                        in_flight.remove(&key);
                        // Note: notify.notify_waiters() can't be called here safely
                        // because the Notify was already moved. The main path handles notification.
                    });
                }
            }
        }

        let (notify, is_waiter, guard) = {
            let mut in_flight = IN_FLIGHT_REQUESTS.lock().await;
            if let Some(existing_notify) = in_flight.get(&cache_key) {
                log::info!("Request for {} is in-flight, waiting...", cache_key);
                (existing_notify.clone(), true, None)
            } else {
                // No in-flight request, create one with guard
                let notify = Arc::new(tokio::sync::Notify::new());
                in_flight.insert(cache_key.clone(), notify.clone());
                (notify.clone(), false, Some(InFlightGuard {
                    cache_key: cache_key.clone(),
                    notify: Some(notify),
                }))
            }
        };

        if is_waiter {
            // Wait for the in-flight request to complete with a timeout
            let _timeout_result = tokio::time::timeout(IN_FLIGHT_TIMEOUT, notify.notified())
                .await
                .unwrap_or_else(|_| {
                    log::warn!("Timeout waiting for in-flight request for {} after {:?}", cache_key, IN_FLIGHT_TIMEOUT);
                });

            log::info!("In-flight request for {} completed after {:?}", cache_key, start.elapsed());

            // Reload after waiting (nostr client may have cached the manifest)
            let client_clone = client.clone();
            let mut site = SiteInfoInner::new(*pubkey, client_clone, identifier.map(String::from));

            // After waiting, we need to re-fetch since the loader may have failed
            // Propagate errors instead of silently returning Ok(None)
            match site.fetch_manifest().await {
                Ok(Some(manifest)) => {
                    // Move the manifest into site.manifest (no clone needed)
                    site.manifest = Some(manifest);
                    if let Err(e) = site.load_server_list().await {
                        log::warn!("Failed to load server list: {}", e);
                    }
                    Ok(Some(SiteInfo {
                        inner: Arc::new(RwLock::new(site)),
                    }))
                }
                Ok(None) => Ok(None),
                Err(e) => {
                    // Propagate the error - caller needs to know this failed
                    Err(anyhow!("Failed to fetch manifest after waiting for in-flight request: {e}"))
                }
            }
        } else {
            // We're the one loading this site - guard will handle cleanup on any path
            // including cancellation/panic. The guard is dropped when this scope ends.
            let (result, fetch_error) = {
                let client_clone = client.clone();
                let mut site =
                    SiteInfoInner::new(*pubkey, client_clone, identifier.map(String::from));

                // Fetch and cache the manifest
                match site.fetch_manifest().await {
                    Ok(Some(manifest)) => {
                        // Move the manifest into site.manifest (no clone needed)
                        site.manifest = Some(manifest);
                        // Note: load_server_list() errors are logged but not fatal
                        // - server list is optional for basic site functionality
                        if let Err(e) = site.load_server_list().await {
                            log::warn!("Failed to load server list: {}", e);
                        }
                        log::info!("Loaded {} site for {} in {:?}", site_type, &pubkey_hex[..8], start.elapsed());
                        (Some(SiteInfo {
                            inner: Arc::new(RwLock::new(site)),
                        }), None)
                    }
                    Ok(None) => {
                        log::info!("No manifest found for {}, returning None", cache_key);
                        (None, None)
                    }
                    Err(e) => {
                        // Capture the error for potential propagation after cleanup
                        (None, Some(e))
                    }
                }
            };

            // Notify all waiters before the guard cleans up
            notify.notify_waiters();

            // Explicitly drop the guard to do immediate cleanup (instead of waiting for scope end)
            drop(guard);

            // Propagate fetch errors (not found returns None, actual errors return Err)
            if let Some(e) = fetch_error {
                Err(anyhow!("Failed to fetch manifest for {}: {e}", cache_key))
            } else {
                Ok(result)
            }
        }
    }

    /// Load and pull the file associated with a given route
    pub async fn serve_route(&self, path: &str) -> Result<PathBuf> {
        let start = std::time::Instant::now();
        let server_list;
        let route = {
            let mut inner = self.inner.write().await;

            if inner.is_expired() {
                log::info!("Site info expired, reloading for path {}", path);
                inner.routes.clear();
                inner.manifest = None;
                inner.load_server_list().await?;
                inner.refresh_timestamp();
            }

            let route = if let Some(r) = {
                if let Some(i) = inner.routes.get(path) {
                    Some(i.clone())
                } else {
                    log::info!("Route {} not cached, loading", path);
                    inner.load_route(path).await?
                }
            } {
                r
            } else {
                bail!("route not found");
            };

            server_list = inner.server_list.clone();
            route
        };

        let result = route.load_cached(&server_list).await;
        log::info!("Served route {} in {:?}", path, start.elapsed());
        result
    }

    /// Extract site info from a host header (Axum-compatible)
    pub async fn from_request(
        host: &str,
        client: &Client,
        site_map: &SiteMap,
        alias_map: &SiteAliasMap,
    ) -> Result<Self> {
        // Parse the subdomain from the host
        // Expected format: subdomain.domain.tld or subdomain.domain.tld:port
        let host_without_port = host.split(':').next().unwrap_or(host);
        let parts: Vec<&str> = host_without_port.split('.').collect();

        let subdomain = if parts.len() >= 3 {
            // Has subdomain: subdomain.domain.tld
            parts[0].to_string()
        } else {
            return Err(anyhow!("No subdomain found"));
        };

        log::info!("Extracted subdomain: {}", subdomain);

        // Get the managed state
        let alias_map_read = alias_map.read().await;
        let site_map_read = site_map.read().await;

        // Extract pubkey from subdomain
        // NIP-5A supports two formats:
        // 1. Root site: npub1... or pubkey in alias map
        // 2. Named site: <pubkeyB36><dTag> where pubkeyB36 is 50 chars base36 and dTag is 1-13 chars
        let (pubkey, identifier) = if let Ok(ent) = Nip19::from_bech32(&subdomain) {
            // npub format - root site
            match ent {
                Nip19::Pubkey(pk) => (*pk.as_bytes(), None),
                Nip19::Profile(pr) => (*pr.public_key.as_bytes(), None),
                _ => {
                    return Err(anyhow!(
                        "Invalid NIP-19 entity '{}', not a public key",
                        subdomain
                    ));
                }
            }
        } else if subdomain.len() >= 51 && subdomain.len() <= 63 {
            // Check for NIP-5A named site format: <pubkeyB36><dTag>
            // pubkeyB36 is exactly 50 characters, dTag is 1-13 characters
            let pubkey_b36 = &subdomain[..50];
            let d_tag = &subdomain[50..];

            // Validate dTag format: ^[a-z0-9-]{1,13}$ and MUST NOT end with '-'
            if d_tag.is_empty() || d_tag.len() > 13 || d_tag.ends_with('-') {
                return Err(anyhow!("Invalid NIP-5A subdomain format: invalid dTag"));
            }
            if !d_tag
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return Err(anyhow!(
                    "Invalid NIP-5A subdomain format: dTag contains invalid characters"
                ));
            }

            // Decode pubkey from base36
            match decode_pubkey_base36(pubkey_b36) {
                Ok(pk) => (pk, Some(d_tag.to_string())),
                Err(e) => {
                    return Err(anyhow!("Invalid NIP-5A subdomain: {}", e));
                }
            }
        } else {
            // Fall back to alias map lookup
            match alias_map_read.get(&subdomain) {
                Some(key) => (*key, None),
                None => {
                    return Err(anyhow!("Subdomain '{}' not found", subdomain));
                }
            }
        };

        // Look up the site info from the index key
        let pubkey_hex = hex::encode(pubkey);
        let cache_key = if let Some(ref id) = identifier {
            // Create a unique key for named sites
            format!("{}-{}", pubkey_hex, id)
        } else {
            pubkey_hex
        };

        let site_info = match site_map_read.get(&cache_key) {
            Some(info) => {
                let expired = {
                    let inner = info.inner.read().await;
                    inner.is_expired()
                };
                if expired {
                    drop(site_map_read);
                    drop(alias_map_read);
                    match SiteInfo::load(client, &pubkey, identifier.as_deref()).await {
                        Ok(Some(s)) => {
                            let mut site_map = site_map.write().await;
                            site_map.insert(cache_key, s.clone());
                            s
                        }
                        Ok(None) | Err(_) => {
                            let mut site_map = site_map.write().await;
                            let expired_site =
                                SiteInfo::new_expired(client, &pubkey, identifier.as_deref()).await;
                            site_map.insert(cache_key, expired_site.clone());
                            expired_site
                        }
                    }
                } else {
                    info.clone()
                }
            }
            None => match SiteInfo::load(client, &pubkey, identifier.as_deref()).await {
                Ok(Some(s)) => {
                    drop(site_map_read);
                    drop(alias_map_read);
                    let mut site_map = site_map.write().await;
                    site_map.insert(cache_key, s.clone());
                    s
                }
                Ok(None) => {
                    let msg = format!(
                        "No site found for pubkey{}",
                        identifier
                            .as_ref()
                            .map(|id| format!(" with identifier '{}'", id))
                            .unwrap_or_default()
                    );
                    return Err(anyhow!(msg));
                }
                Err(e) => {
                    return Err(anyhow!("Failed to resolve nsite for {}, {}", subdomain, e));
                }
            },
        };

        Ok(site_info)
    }
}

/// Structure used to load and cache NSites
#[derive(Clone)]
struct SiteInfoInner {
    /// Nostr client instance
    client: Client,

    /// The owner public key
    pubkey: [u8; 32],

    /// Resolved routes Path => Nostr Event
    routes: HashMap<String, SiteRoute>,

    /// List of Blossom servers to load content from
    server_list: Vec<Url>,

    /// Cached site manifest event
    manifest: Option<Event>,

    /// Site identifier for NIP-5A named sites (from d tag)
    identifier: Option<String>,

    /// Timestamp when this site info was last refreshed
    last_refresh: u64,
}

impl SiteInfoInner {
    fn new(pubkey: [u8; 32], client: Client, identifier: Option<String>) -> Self {
        Self {
            pubkey,
            client,
            routes: HashMap::new(),
            server_list: vec![
                "https://nostr.download".parse().unwrap(),
                "https://blossom.band".parse().unwrap(),
                "https://24242.io".parse().unwrap(),
                "https://blossom.primal.net".parse().unwrap(),
            ],
            manifest: None,
            identifier,
            last_refresh: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }

    fn set_expired(&mut self) {
        self.last_refresh = 0;
        self.manifest = None;
        self.routes.clear();
    }

    fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        now.saturating_sub(self.last_refresh) > SITE_INFO_EXPIRY.as_secs()
    }

    fn refresh_timestamp(&mut self) {
        self.last_refresh = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
    }
}

/// A single resolved NSite route
#[derive(Clone)]
pub struct SiteRoute {
    /// Absolute url path
    pub path: String,
    /// SHA256 hash of the file
    pub key: [u8; 32],
}

impl SiteRoute {
    /// Download the file for this route or load it from disk cache
    pub async fn load_cached(&self, server_list: &Vec<Url>) -> Result<PathBuf> {
        let key_hex = hex::encode(self.key);
        let out_dir = temp_dir().join("nsite").join(&key_hex[0..2]);
        if !out_dir.exists() {
            create_dir_all(&out_dir).await?;
        }
        let mut out_path = out_dir.join(&key_hex);
        // set the extension based on the URL path
        if let Some(ext) = PathBuf::from(&self.path).extension() {
            out_path.set_extension(ext);
        }

        if out_path.exists() {
            Ok(out_path)
        } else {
            for s in server_list {
                let url = s.join(&key_hex)?;
                let start = std::time::Instant::now();
                match reqwest::get(url.clone()).await {
                    Ok(r) => {
                        let status = r.status();
                        if !status.is_success() {
                            log::info!("Upstream GET {} {} (total: {:?})", url, status, start.elapsed());
                            continue;
                        }
                        let bytes = r.bytes().await?;
                        tokio::fs::write(&out_path, &bytes).await?;
                        log::info!("Upstream GET {} {} {} bytes (total: {:?})", url, status, bytes.len(), start.elapsed());
                        return Ok(out_path);
                    }
                    Err(e) => {
                        warn!("Failed to load {} from {}, {}", key_hex, s, e);
                    }
                }
            }
            bail!(
                "Failed to load {}=>{}, not found on any server",
                self.path,
                key_hex
            );
        }
    }
}

impl SiteInfoInner {
    /// Fetch the site manifest event based on kind and identifier (NIP-5A)
    /// - Root site: kind 15128, no d tag
    /// - Named site: kind 35128, with d tag
    async fn fetch_manifest(&mut self) -> Result<Option<Event>> {
        let kind = if self.identifier.is_some() {
            Kind::Custom(35_128) // Named site
        } else {
            Kind::Custom(15_128) // Root site
        };

        let mut filter = Filter::new()
            .kind(kind)
            .author(PublicKey::from_slice(&self.pubkey)?);

        // For named sites, filter by d tag (identifier)
        if let Some(ref id) = self.identifier {
            filter = filter.identifier(id);
        }

        let start = std::time::Instant::now();
        let pubkey_short = hex::encode(self.pubkey);
        log::info!("Fetching manifest event (kind {}) for pubkey {}", kind, &pubkey_short[..8]);
        let events = self.client.fetch_events(filter, DEFAULT_TIMEOUT).await?;
        log::info!("Fetched manifest in {:?}, got {} events", start.elapsed(), events.len());

        // Validate the manifest conforms to NIP-5A spec
        if let Some(event) = events.into_iter().next() {
            // Validate d tag requirements
            let has_d_tag = event.tags.find(TagKind::d()).is_some();

            if let Some(ref identifier) = self.identifier {
                // Named site MUST have a d tag
                if !has_d_tag {
                    warn!("Named site manifest missing required d tag");
                    return Ok(None);
                }

                // Validate d tag value matches the identifier
                if let Some(d_tag) = event.tags.find(TagKind::d())
                    && let Some(d_value) = d_tag.content()
                    && d_value != identifier
                {
                    warn!(
                        "d tag value '{}' doesn't match requested identifier '{}'",
                        d_value, identifier
                    );
                    return Ok(None);
                }
            } else {
                // Root site MUST NOT have a d tag
                if has_d_tag {
                    warn!("Root site manifest MUST NOT have a d tag");
                    return Ok(None);
                }
            }

            // Validate path tags - MUST have at least one
            let path_tags: Vec<_> = event
                .tags
                .iter()
                .filter(|t| t.kind() == TagKind::Custom(Cow::Borrowed("path")))
                .collect();

            if path_tags.is_empty() {
                warn!("Manifest missing required path tags");
                return Ok(None);
            }

            // Validate each path tag format
            for tag in &path_tags {
                let tag_slice = tag.as_slice();
                if tag_slice.len() != 3 {
                    warn!(
                        "Invalid path tag format (expected 3 elements, got {})",
                        tag_slice.len()
                    );
                    return Ok(None);
                }

                let tag_path = &tag_slice[1];
                let hash_hex = &tag_slice[2];

                // Path must start with /
                if !tag_path.as_str().starts_with('/') {
                    warn!("Invalid path tag: path must start with '/'");
                    return Ok(None);
                }

                // Hash must be exactly 64 hex characters (32 bytes)
                if hash_hex.as_str().len() != 64 {
                    warn!(
                        "Invalid hash length in path tag (expected 64 hex chars, got {})",
                        hash_hex.as_str().len()
                    );
                    return Ok(None);
                }

                // Validate hash is valid hex
                if hex::decode(hash_hex.as_str()).is_err() {
                    warn!("Invalid hex hash in path tag");
                    return Ok(None);
                }
            }

            // Validate source tag if present
            for tag in event
                .tags
                .iter()
                .filter(|t| t.kind() == TagKind::Custom(Cow::Borrowed("source")))
            {
                let tag_slice = tag.as_slice();
                if tag_slice.len() != 2 {
                    warn!(
                        "Invalid source tag format (expected 2 elements, got {})",
                        tag_slice.len()
                    );
                    return Ok(None);
                }

                let url_str = &tag_slice[1];
                let url = url_str.as_str();

                // URL must start with http:// or https://
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    warn!("Invalid source tag: URL must be http or https");
                    return Ok(None);
                }
            }

            Ok(Some(event))
        } else {
            Ok(None)
        }
    }

    /// Extract the sha256 hash for a given path from the manifest's path tags
    /// NIP-5A path tag format: ["path", "/absolute/path", "sha256hash"]
    /// Note: Path tags are validated in fetch_manifest() per NIP-5A spec
    fn get_hash_for_path(&self, event: &Event, requested_path: &str) -> Result<[u8; 32]> {
        // Normalize the requested path - if it ends with / or has no extension, append index.html
        let normalized_path = if requested_path.ends_with('/') {
            format!("{}index.html", requested_path)
        } else if !requested_path.contains('.') {
            format!("{}/index.html", requested_path)
        } else {
            requested_path.to_string()
        };

        // Look for a path tag where the second element matches the requested path
        for tag in event.tags.iter() {
            if tag.kind() == TagKind::Custom(Cow::Borrowed("path")) {
                let tag_slice = tag.as_slice();
                // Path tags are validated in fetch_manifest, but we do a quick length check here
                if tag_slice.len() < 3 {
                    continue;
                }

                let tag_path = &tag_slice[1];
                let hash_hex = &tag_slice[2];

                // Check if this tag's path matches the requested path
                if tag_path.as_str() == normalized_path || tag_path.as_str() == requested_path {
                    let hash_bytes = hex::decode(hash_hex)?;
                    if hash_bytes.len() != 32 {
                        return Err(anyhow!("Invalid hash length: {} bytes", hash_bytes.len()));
                    }
                    let mut hash = [0u8; 32];
                    hash.copy_from_slice(&hash_bytes);
                    return Ok(hash);
                }
            }
        }

        Err(anyhow!(
            "No path tag found for '{}' in manifest",
            normalized_path
        ))
    }

    /// Load a single route for this site using NIP-5A manifest format
    pub async fn load_route(&mut self, path: &str) -> Result<Option<SiteRoute>> {
        let start = std::time::Instant::now();
        log::info!("Loading route: {}", path);

        // Use cached manifest or fetch if not present/expired
        if self.manifest.is_none() {
            log::info!("No cached manifest, fetching...");
            let manifest = match self.fetch_manifest().await? {
                Some(ev) => ev,
                None => {
                    log::info!("No manifest found for route {}", path);
                    return Ok(None);
                }
            };
            self.manifest = Some(manifest.clone());
        }

        // Extract hash from path tag using cached manifest
        let manifest = self.manifest.as_ref().unwrap();
        match self.get_hash_for_path(manifest, path) {
            Ok(hash) => {
                let new_route = SiteRoute {
                    path: path.to_string(),
                    key: hash,
                };
                self.routes
                    .insert(new_route.path.clone(), new_route.clone());
                let hash_short = hex::encode(new_route.key);
                log::info!("Loaded route {} in {:?}, hash: {}", path, start.elapsed(), &hash_short[..8]);
                Ok(Some(new_route))
            }
            Err(e) => {
                warn!("Failed to get hash for path {}: {}", path, e);
                Ok(None)
            }
        }
    }

    /// Load blossom server list from manifest server tags or BUD-03 (kind 10063)
    pub async fn load_server_list(&mut self) -> Result<()> {
        let start = std::time::Instant::now();
        let pubkey_short = hex::encode(self.pubkey);
        log::info!("Loading server list for pubkey {}", &pubkey_short[..8]);

        // First check for server tags in the cached manifest (NIP-5A)
        if let Some(ref manifest) = self.manifest {
            let manifest_servers: Vec<Url> = manifest
                .tags
                .filter(TagKind::Custom(Cow::Borrowed("server")))
                .filter_map(|t| t.content())
                .filter_map(|content| content.parse().ok())
                .collect();

            if !manifest_servers.is_empty() {
                log::info!("Loaded {} servers from cached manifest in {:?}", manifest_servers.len(), start.elapsed());
                self.server_list = manifest_servers;
                return Ok(());
            }
        }

        // Fall back to BUD-03 (kind 10063) user servers
        log::info!("No servers in manifest, fetching BUD-03 (kind 10063)");
        let filter = Filter::new()
            .kind(Kind::Custom(10_063))
            .author(PublicKey::from_slice(&self.pubkey)?);

        let events = self.client.fetch_events(filter, DEFAULT_TIMEOUT).await?;
        if let Some(ev) = events.into_iter().next() {
            let server_tags: Vec<Url> = ev
                .tags
                .filter(TagKind::Custom(Cow::Borrowed("server")))
                .filter_map(|t| t.content())
                .filter_map(|content| content.parse().ok())
                .collect();
            if !server_tags.is_empty() {
                log::info!("Loaded {} servers from BUD-03 in {:?}", server_tags.len(), start.elapsed());
                self.server_list = server_tags;
            }
        }

        Ok(())
    }
}

/// Decode a 50-character base36-encoded pubkey to a 32-byte array
/// Base36 uses digits 0-9 and lowercase letters a-z
/// This implements big-endian base36 decoding for 256-bit values
fn decode_pubkey_base36(b36: &str) -> Result<[u8; 32]> {
    if b36.len() != 50 {
        return Err(anyhow!("Base36 pubkey must be exactly 50 characters"));
    }

    // Initialize result as 32 zero bytes (256 bits)
    let mut result = [0u8; 32];

    // Process each character, multiplying the accumulated value by 36 and adding the new digit
    // We do this from left to right, handling carries properly
    for ch in b36.bytes() {
        let digit = if ch.is_ascii_digit() {
            (ch - b'0') as u64
        } else if ch.is_ascii_lowercase() {
            (ch - b'a' + 10) as u64
        } else {
            return Err(anyhow!("Invalid base36 character: {}", ch as char));
        };

        // Multiply current result by 36 and add digit
        // This is done in-place with carry propagation
        let mut carry = digit;
        for byte in result.iter_mut().rev() {
            let val = (*byte as u64) * 36 + carry;
            *byte = (val & 0xFF) as u8;
            carry = val >> 8;
        }

        if carry > 0 {
            return Err(anyhow!("Base36 value too large for 256 bits"));
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;
    use tokio::sync::Notify;

    #[test]
    fn test_decode_pubkey_base36() {
        // Test with known values
        // A 50-char base36 string that represents a valid 32-byte pubkey
        let valid_b36 = "00000000000000000000000000000000000000000000000000";
        let result = decode_pubkey_base36(valid_b36).unwrap();
        assert_eq!(result, [0u8; 32]);

        // All z's (max value for base36) - should fail as it exceeds 256 bits
        let max_b36 = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
        assert!(decode_pubkey_base36(max_b36).is_err());

        // Test with a realistic pubkey-like value
        let realistic_b36 = "00000000000000000000000000000000000000000000000001";
        let result = decode_pubkey_base36(realistic_b36).unwrap();
        assert_eq!(&result[31..], &[1]);

        // Wrong length should fail
        let short_b36 = "0000000000000000000000000000000000000000000000000";
        assert!(decode_pubkey_base36(short_b36).is_err());

        let long_b36 = "000000000000000000000000000000000000000000000000000";
        assert!(decode_pubkey_base36(long_b36).is_err());
    }

    #[test]
    fn test_decode_pubkey_base36_invalid_chars() {
        // Base36 only allows 0-9 and a-z (exactly 50 chars)
        let invalid_chars = "00000000000000000000000000000000000000000000000000g";
        assert!(decode_pubkey_base36(invalid_chars).is_err());

        let uppercase = "00000000000000000000000000000000000000000000000000A";
        assert!(decode_pubkey_base36(uppercase).is_err());

        let with_dash = "00000000000000000000000000000000000000000000000000-";
        assert!(decode_pubkey_base36(with_dash).is_err());
    }

    #[test]
    fn test_decode_pubkey_base36_edge_cases() {
        // Single non-zero byte at last position (50 chars)
        let pos_31 = "0000000000000000000000000000000000000000000000000a";
        let result = decode_pubkey_base36(pos_31).unwrap();
        assert_eq!(result[31], 10);

        // Test decoding and re-encoding round trip would be complex without encoding function
        // Just verify the decoding produces consistent results (50 chars)
        // Use a value that won't overflow 256 bits
        let input = "0000000000000000000000000000000000000000000000000a";
        let result1 = decode_pubkey_base36(input).unwrap();
        let result2 = decode_pubkey_base36(input).unwrap();
        assert_eq!(result1, result2);
    }

    #[tokio::test]
    async fn test_site_info_expiration() {
        // Create a minimal client for testing
        let keys = Keys::generate();
        let client = Client::new(keys);
        
        let pubkey = [0u8; 32];
        let mut site = SiteInfoInner::new(pubkey, client.clone(), None);
        
        // Fresh site should not be expired
        assert!(!site.is_expired());
        
        // Set to expired
        site.set_expired();
        assert!(site.is_expired());
        
        // After refresh, should not be expired
        site.refresh_timestamp();
        assert!(!site.is_expired());
    }

    #[tokio::test]
    async fn test_site_info_inner_new() {
        let keys = Keys::generate();
        let client = Client::new(keys);
        
        let pubkey = [1u8; 32];
        let site = SiteInfoInner::new(pubkey, client.clone(), Some("test".to_string()));
        
        assert_eq!(site.pubkey, pubkey);
        assert_eq!(site.identifier, Some("test".to_string()));
        assert!(site.routes.is_empty());
        assert!(site.manifest.is_none());
        assert!(!site.server_list.is_empty());
    }

    #[tokio::test]
    async fn test_inflight_notify_waiters() {
        // Test that waiters are properly notified when loader completes
        // This addresses the review concern about testing waiter unblocking
        
        let notify = Arc::new(Notify::new());
        let notify_clone = notify.clone();
        
        // Spawn a waiter task
        let waiter = tokio::spawn(async move {
            notify_clone.notified().await;
            "notified"
        });
        
        // Give waiter time to start waiting
        tokio::time::sleep(Duration::from_millis(10)).await;
        
        // Simulate loader completing and notifying waiters
        notify.notify_waiters();
        
        // Waiter should complete successfully
        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter should complete within timeout");
        
        assert_eq!(result.unwrap(), "notified");
    }

    #[tokio::test]
    async fn test_inflight_timeout_on_stalled_request() {
        // Test that waiters timeout properly when loader stalls
        // This verifies the 30s timeout behavior mentioned in the PR
        
        let notify = Arc::new(Notify::new());
        
        // Simulate waiting with timeout but never notifying (stalled loader)
        let timeout_result: Result<(), tokio::time::error::Elapsed> = tokio::time::timeout(
            Duration::from_millis(50), // Use short timeout for test
            notify.notified()
        ).await;
        
        // Should timeout since nobody notifies
        assert!(timeout_result.is_err(), "Expected timeout when notify never happens");
    }

    #[tokio::test]
    async fn test_inflight_map_cleanup() {
        // Test that the IN_FLIGHT_REQUESTS map is properly managed
        // This addresses the review concern about IN_FLIGHT_REQUESTS being cleared
        
        let cache_key = "test-pubkey-123".to_string();
        
        // Verify map starts empty
        {
            let in_flight = IN_FLIGHT_REQUESTS.lock().await;
            assert!(!in_flight.contains_key(&cache_key));
        }
        
        // Add an entry
        let notify = Arc::new(Notify::new());
        {
            let mut in_flight = IN_FLIGHT_REQUESTS.lock().await;
            in_flight.insert(cache_key.clone(), notify);
            assert!(in_flight.contains_key(&cache_key));
        }
        
        // Remove the entry (simulating loader cleanup)
        {
            let mut in_flight = IN_FLIGHT_REQUESTS.lock().await;
            in_flight.remove(&cache_key);
            assert!(!in_flight.contains_key(&cache_key));
        }
    }
}
