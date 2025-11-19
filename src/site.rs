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
    pub async fn load(client: &Client, pubkey: &[u8; 32]) -> Result<Option<Self>> {
        let mut site = SiteInfoInner {
            pubkey: pubkey.clone(),
            client: client.clone(),
            routes: HashMap::new(),
            server_list: vec![
                "https://nostr.download".parse()?,
                "https://blossom.band".parse()?,
                "https://24242.io".parse()?,
                "https://blossom.primal.net".parse()?,
            ],
        };
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
    /// Load a single route for this site
    pub async fn load_route(&mut self, path: &str) -> Result<Option<SiteRoute>> {
        // TODO: clean path
        let filter = Filter::new()
            .kind(Kind::Custom(34_128))
            .author(PublicKey::from_slice(&self.pubkey)?)
            .identifier(path);
        let events = self.client.fetch_events(filter, DEFAULT_TIMEOUT).await?;
        if let Some(ev) = events.into_iter().next() {
            let x_tag: [u8; 32] = ev
                .tags
                .find(TagKind::Custom(Cow::Borrowed("x")))
                .and_then(|t| t.content())
                .and_then(|t| hex::decode(t).ok())
                .and_then(|t| t.try_into().ok())
                .ok_or(anyhow::anyhow!(
                    "Invalid NSite event at path {}, missing or invalid x tag",
                    path
                ))?;

            let new_route = SiteRoute {
                path: path.to_string(),
                key: x_tag,
                created_at: ev.created_at.as_secs(),
            };
            self.routes
                .insert(new_route.path.clone(), new_route.clone());
            Ok(Some(new_route))
        } else {
            Ok(None)
        }
    }

    /// Load blossom server list
    pub async fn load_server_list(&mut self) -> Result<()> {
        let filter = Filter::new()
            .kind(Kind::Custom(10_063))
            .author(PublicKey::from_slice(&self.pubkey)?);

        let events = self.client.fetch_events(filter, DEFAULT_TIMEOUT).await?;
        if let Some(ev) = events.into_iter().next() {
            let server_tags = ev
                .tags
                .filter(TagKind::Custom(Cow::Borrowed("server")))
                .filter_map(|t| t.content().map(Url::parse))
                .filter_map(|url| url.ok())
                .collect::<Vec<_>>();
            self.server_list = server_tags;
        }

        Ok(())
    }
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
        // TODO: resolve nip5
        let pubkey = if let Ok(ent) = Nip19::from_bech32(&subdomain) {
            match ent {
                Nip19::Pubkey(pk) => pk.as_bytes().clone(),
                Nip19::Profile(pr) => pr.public_key.as_bytes().clone(),
                _ => {
                    return Outcome::Error((
                        Status::NotFound,
                        anyhow!("Invalid NIP-19 entity '{}', not a public key", subdomain),
                    ));
                }
            }
        } else {
            let alias_map_read = alias_map.read().await;
            match alias_map_read.get(&subdomain) {
                Some(key) => *key,
                None => {
                    return Outcome::Error((
                        Status::NotFound,
                        anyhow!("Subdomain '{}' not found in alias map", subdomain),
                    ));
                }
            }
        };

        // Look up the site info from the index key
        let site_info = {
            let site_map_read = site_map.read().await;
            match site_map_read.get(&pubkey) {
                Some(info) => info.clone(),
                None => match SiteInfo::load(client, &pubkey).await {
                    Ok(Some(s)) => {
                        drop(site_map_read);
                        let mut site_map = site_map.write().await;
                        site_map.insert(pubkey.clone(), s.clone());
                        s
                    }
                    Ok(None) => {
                        return Outcome::Error((
                            Status::NotFound,
                            anyhow!("No site found for pubkey"),
                        ));
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
