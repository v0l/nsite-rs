use crate::site::SiteInfo;
use anyhow::Result;
use clap::Parser;
use log::{error, info};
use nostr_sdk::Client;
use rocket::fs::NamedFile;
use rocket::http::ContentType;
use rocket::{Config, Either, Rocket, routes};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

mod site;

type SiteMap = Arc<RwLock<HashMap<[u8; 32], SiteInfo>>>;
type SiteAliasMap = Arc<RwLock<HashMap<String, [u8; 32]>>>;

/// NSite proxy
#[derive(Parser)]
#[clap(version, about)]
struct Args {
    #[arg(long, short)]
    pub relay: Vec<String>,
}

#[rocket::main]
async fn main() -> Result<()> {
    env_logger::init();

    let mut args = Args::parse();
    let client = Client::builder().build();

    if args.relay.is_empty() {
        args.relay = vec![
            "wss://relay.damus.io".to_string(),
            "wss://relay.snort.social".to_string(),
            "wss://relay.primal.net".to_string(),
            "wss://nos.lol".to_string(),
        ];
    }
    for r in args.relay {
        info!("Connecting to {}", r);
        client.add_relay(r).await?;
    }
    client.connect().await;

    let site_map = SiteMap::default();
    let site_alias_map = SiteAliasMap::default();

    let mut config = Config::default();
    config.address = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

    Rocket::custom(config)
        .manage(site_map)
        .manage(site_alias_map)
        .manage(client)
        .mount("/", routes![serve_site])
        .launch()
        .await?;

    Ok(())
}

#[rocket::get("/<path..>", rank = 1)]
async fn serve_site(
    path: PathBuf,
    site: Option<SiteInfo>,
) -> Option<Either<NamedFile, (ContentType, &'static str)>> {
    if let Some(site) = site {
        let path_str = path.display().to_string();
        let path = if path_str == "" {
            "/index.html".to_string()
        } else {
            format!("/{}", path_str)
        };
        match site.serve_route(&path).await {
            Ok(f) => NamedFile::open(f).await.ok().map(Either::Left),
            Err(e) => {
                error!("Failed to open route: {}", e);
                None
            }
        }
    } else {
        Some(Either::Right((
            ContentType::HTML,
            include_str!("index.html"),
        )))
    }
}
