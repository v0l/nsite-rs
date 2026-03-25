use crate::{SiteAliasMap, SiteMap};
use anyhow::{Result, anyhow, bail};
use log::warn;
use nostr_sdk::prelude::Nip19;
use nostr_sdk::{Client, Event, Filter, FromBech32, Kind, PublicKey, TagKind, Url};
use rocket::Request;
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome};
use std::borrow::Cow;
use std::collections::HashMap;
use std::env::temp_dir;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs::create_dir_all;
use tokio::sync::RwLock;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct SiteInfo {
    /// Owner public key
    pub pubkey: PublicKey,
    inner: Arc<RwLock<SiteInfoInner>>,
}

impl SiteInfo {
    /// Load site info for a pubkey
    /// For root sites: identifier is None
    /// For named sites (NIP-5A): identifier is the d tag value
    pub async fn load(client: &Client, pubkey: &[u8; 32], identifier: Option<&str>) -> Result<Option<Self>> {
        let client_clone = client.clone();
        let mut site = SiteInfoInner::new(pubkey.clone(), client_clone, identifier.map(String::from));
        
        // Try to load index.html to verify the site exists
        if site.load_route("/index.html").await?.is_none() {
            return Ok(None);
        }
        site.load_server_list().await?;
        Ok(Some(SiteInfo {
            pubkey: PublicKey::from_slice(pubkey)?,
            inner: Arc::new(RwLock::new(site)),
        }))
    }

    /// Load a single route for this site
    pub async fn load_route(&self, path: &str) -> Result<Option<SiteRoute>> {
        let mut inner = self.inner.write().await;
        inner.load_route(path).await
    }

    /// Load blossom server list
    pub async fn load_server_list(&self) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner.load_server_list().await
    }

    pub async fn get_route(&self, path: &str) -> Option<SiteRoute> {
        let inner = self.inner.read().await;
        inner.routes.get(path).cloned()
    }

    /// Load and pull the file associated with a given route
    pub async fn serve_route(&self, path: &str) -> Result<PathBuf> {
        let mut inner = self.inner.write().await;
        let route = if let Some(r) = {
            if let Some(i) = inner.routes.get(path) {
                Some(i.clone())
            } else {
                inner.load_route(path).await?
            }
        } {
            r
        } else {
            bail!("route not found");
        };

        route.load_cached(&inner.server_list).await
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

    /// Site identifier for NIP-5A named sites (from d tag)
    identifier: Option<String>,
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
            identifier,
        }
    }
}

/// A single resolved NSite route
#[derive(Clone)]
pub struct SiteRoute {
    /// Absolute url path
    pub path: String,
    /// SHA256 hash of the file
    pub key: [u8; 32],
    /// Timestamp when the site route event was created
    pub created_at: u64,
}

impl SiteRoute {
    /// Download the file for this route or load it from disk cache
    pub async fn load_cached(&self, server_list: &Vec<Url>) -> Result<PathBuf> {
        let key_hex = hex::encode(&self.key);
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
            // try to cache data from server list
            for s in server_list {
                match reqwest::get(s.join(&key_hex)?).await {
                    Ok(r) => {
                        if !r.status().is_success() {
                            continue;
                        }
                        tokio::fs::write(&out_path, r.bytes().await?).await?;
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
            Kind::Custom(35_128)  // Named site
        } else {
            Kind::Custom(15_128)  // Root site
        };

        let mut filter = Filter::new()
            .kind(kind)
            .author(PublicKey::from_slice(&self.pubkey)?);

        // For named sites, filter by d tag (identifier)
        if let Some(ref id) = self.identifier {
            filter = filter.identifier(id);
        }

        let events = self.client.fetch_events(filter, DEFAULT_TIMEOUT).await?;
        
        // Validate the manifest conforms to NIP-5A spec
        if let Some(event) = events.into_iter().next() {
            // Validate d tag requirements
            let has_d_tag = event.tags.find(TagKind::d()).is_some();
            
            if self.identifier.is_some() {
                // Named site MUST have a d tag
                if !has_d_tag {
                    warn!("Named site manifest missing required d tag");
                    return Ok(None);
                }
                
                // Validate d tag value matches the identifier
                if let Some(d_tag) = event.tags.find(TagKind::d()) {
                    if let Some(d_value) = d_tag.content() {
                        if d_value != self.identifier.as_ref().unwrap() {
                            warn!("d tag value '{}' doesn't match requested identifier '{}'", 
                                  d_value, self.identifier.as_ref().unwrap());
                            return Ok(None);
                        }
                    }
                }
            } else {
                // Root site MUST NOT have a d tag
                if has_d_tag {
                    warn!("Root site manifest MUST NOT have a d tag");
                    return Ok(None);
                }
            }
            
            // Validate path tags - MUST have at least one
            let path_tags: Vec<_> = event.tags
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
                    warn!("Invalid path tag format (expected 3 elements, got {})", tag_slice.len());
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
                    warn!("Invalid hash length in path tag (expected 64 hex chars, got {})", 
                          hash_hex.as_str().len());
                    return Ok(None);
                }
                
                // Validate hash is valid hex
                if hex::decode(hash_hex.as_str()).is_err() {
                    warn!("Invalid hex hash in path tag");
                    return Ok(None);
                }
            }
            
            // Validate source tag if present
            for tag in event.tags.iter().filter(|t| t.kind() == TagKind::Custom(Cow::Borrowed("source"))) {
                let tag_slice = tag.as_slice();
                if tag_slice.len() != 2 {
                    warn!("Invalid source tag format (expected 2 elements, got {})", tag_slice.len());
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
                        return Err(anyhow::anyhow!("Invalid hash length: {} bytes", hash_bytes.len()));
                    }
                    let mut hash = [0u8; 32];
                    hash.copy_from_slice(&hash_bytes);
                    return Ok(hash);
                }
            }
        }

        Err(anyhow::anyhow!(
            "No path tag found for '{}' in manifest",
            normalized_path
        ))
    }

    /// Load a single route for this site using NIP-5A manifest format
    pub async fn load_route(&mut self, path: &str) -> Result<Option<SiteRoute>> {
        // Fetch the site manifest (NIP-5A format)
        let manifest = match self.fetch_manifest().await? {
            Some(ev) => ev,
            None => return Ok(None),
        };

        // Extract hash from path tag
        match self.get_hash_for_path(&manifest, path) {
            Ok(hash) => {
                let new_route = SiteRoute {
                    path: path.to_string(),
                    key: hash,
                    created_at: manifest.created_at.as_secs(),
                };
                self.routes
                    .insert(new_route.path.clone(), new_route.clone());
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
        // First check for server tags in the manifest (NIP-5A)
        if let Some(manifest) = self.fetch_manifest().await? {
            let manifest_servers: Vec<Url> = manifest
                .tags
                .filter(TagKind::Custom(Cow::Borrowed("server")))
                .filter_map(|t| t.content())
                .filter_map(|content| content.parse().ok())
                .collect();

            if !manifest_servers.is_empty() {
                self.server_list = manifest_servers;
                return Ok(());
            }
        }

        // Fall back to BUD-03 (kind 10063) user servers
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
        return Err(anyhow::anyhow!("Base36 pubkey must be exactly 50 characters"));
    }

    // Initialize result as 32 zero bytes (256 bits)
    let mut result = [0u8; 32];

    // Process each character, multiplying the accumulated value by 36 and adding the new digit
    // We do this from left to right, handling carries properly
    for ch in b36.bytes() {
        let digit = if ch >= b'0' && ch <= b'9' {
            (ch - b'0') as u64
        } else if ch >= b'a' && ch <= b'z' {
            (ch - b'a' + 10) as u64
        } else {
            return Err(anyhow::anyhow!("Invalid base36 character: {}", ch as char));
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
            return Err(anyhow::anyhow!("Base36 value too large for 256 bits"));
        }
    }

    Ok(result)
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for SiteInfo {
    type Error = anyhow::Error;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        // Extract the Host header
        let host = match request.headers().get_one("Host") {
            Some(h) => h,
            None => {
                return Outcome::Error((
                    Status::BadRequest,
                    anyhow::anyhow!("Missing Host header"),
                ));
            }
        };

        // Parse the subdomain from the host
        // Expected format: subdomain.domain.tld or subdomain.domain.tld:port
        let host_without_port = host.split(':').next().unwrap_or(host);
        let parts: Vec<&str> = host_without_port.split('.').collect();

        let subdomain = if parts.len() >= 3 {
            // Has subdomain: subdomain.domain.tld
            parts[0].to_string()
        } else {
            return Outcome::Forward(Status::Ok);
        };

        log::debug!("Extracted subdomain: {}", subdomain);

        // Get the managed state
        let alias_map = match request.rocket().state::<SiteAliasMap>() {
            Some(map) => map,
            None => {
                return Outcome::Error((
                    Status::InternalServerError,
                    anyhow!("SiteAliasMap not found in managed state"),
                ));
            }
        };

        let site_map = match request.rocket().state::<SiteMap>() {
            Some(map) => map,
            None => {
                return Outcome::Error((
                    Status::InternalServerError,
                    anyhow!("SiteMap not found in managed state"),
                ));
            }
        };

        let client = match request.rocket().state::<Client>() {
            Some(c) => c,
            None => {
                return Outcome::Error((
                    Status::InternalServerError,
                    anyhow!("Client not found in managed state"),
                ));
            }
        };

        // Extract pubkey from subdomain
        // NIP-5A supports two formats:
        // 1. Root site: npub1... or pubkey in alias map
        // 2. Named site: <pubkeyB36><dTag> where pubkeyB36 is 50 chars base36 and dTag is 1-13 chars
        let (pubkey, identifier) = if let Ok(ent) = Nip19::from_bech32(&subdomain) {
            // npub format - root site
            match ent {
                Nip19::Pubkey(pk) => (pk.as_bytes().clone(), None),
                Nip19::Profile(pr) => (pr.public_key.as_bytes().clone(), None),
                _ => {
                    return Outcome::Error((
                        Status::NotFound,
                        anyhow!("Invalid NIP-19 entity '{}', not a public key", subdomain),
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
                return Outcome::Error((
                    Status::NotFound,
                    anyhow!("Invalid NIP-5A subdomain format: invalid dTag"),
                ));
            }
            if !d_tag.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                return Outcome::Error((
                    Status::NotFound,
                    anyhow!("Invalid NIP-5A subdomain format: dTag contains invalid characters"),
                ));
            }

            // Decode pubkey from base36
            match decode_pubkey_base36(pubkey_b36) {
                Ok(pk) => (pk, Some(d_tag.to_string())),
                Err(e) => {
                    return Outcome::Error((
                        Status::NotFound,
                        anyhow!("Invalid NIP-5A subdomain: {}", e),
                    ));
                }
            }
        } else {
            // Fall back to alias map lookup
            let alias_map_read = alias_map.read().await;
            match alias_map_read.get(&subdomain) {
                Some(key) => (*key, None),
                None => {
                    return Outcome::Error((
                        Status::NotFound,
                        anyhow!("Subdomain '{}' not found", subdomain),
                    ));
                }
            }
        };

        // Look up the site info from the index key
        let site_info = {
            let site_map_read = site_map.read().await;
            // For named sites, we need to use identifier as part of the cache key
            let pubkey_hex = hex::encode(pubkey);
            let cache_key = if identifier.is_some() {
                // Create a unique key for named sites
                format!("{}-{}", pubkey_hex, identifier.as_ref().unwrap())
            } else {
                pubkey_hex
            };

            match site_map_read.get(&cache_key) {
                Some(info) => info.clone(),
                None => match SiteInfo::load(client, &pubkey, identifier.as_deref()).await {
                    Ok(Some(s)) => {
                        drop(site_map_read);
                        let mut site_map = site_map.write().await;
                        site_map.insert(cache_key, s.clone());
                        s
                    }
                    Ok(None) => {
                        let msg = format!("No site found for pubkey{}", 
                            identifier.as_ref().map(|id| format!(" with identifier '{}'", id)).unwrap_or_default());
                        return Outcome::Error((Status::NotFound, anyhow!(msg)));
                    }
                    Err(e) => {
                        return Outcome::Error((
                            Status::NotFound,
                            anyhow!("Failed to resolve nsite for {}, {}", subdomain, e),
                        ));
                    }
                },
            }
        };

        Outcome::Success(site_info)
    }
}
