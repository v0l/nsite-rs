use anyhow::Result;
use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::Response,
    routing::get,
};
use clap::Parser;
use log::{error, info};
use nostr_sdk::Client;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;

mod site;

const INDEX_HTML: &str = include_str!("index.html");

type SiteMap = Arc<RwLock<HashMap<String, site::SiteInfo>>>;
type SiteAliasMap = Arc<RwLock<HashMap<String, [u8; 32]>>>;

/// NSite proxy
#[derive(Parser)]
#[clap(version, about)]
struct Args {
    #[arg(long, short)]
    pub relay: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("Failed to install crypto provider"))?;

    let args = Args::parse();
    let client = Client::builder().build();

    let relays = if args.relay.is_empty() {
        vec![
            "wss://relay.damus.io".to_string(),
            "wss://relay.snort.social".to_string(),
            "wss://relay.primal.net".to_string(),
            "wss://nos.lol".to_string(),
        ]
    } else {
        args.relay
    };

    for r in &relays {
        info!("Connecting to {}", r);
        client.add_relay(r).await?;
    }
    client.connect().await;

    let site_map = SiteMap::default();
    let site_alias_map = SiteAliasMap::default();

    let app = Router::new()
        .route("/", get(serve_site))
        .route("/{*path}", get(serve_site))
        .layer(CorsLayer::permissive())
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state((site_map, site_alias_map, client));

    let addr = SocketAddr::from((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 3000));
    info!("Listening on {}", addr);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn serve_site(
    State((site_map, site_alias_map, client)): State<(SiteMap, SiteAliasMap, Client)>,
    request: axum::extract::Request,
) -> Result<Response, StatusCode> {
    let path_str = request.uri().path().trim_start_matches('/');
    let path_buf = if path_str.is_empty() {
        "index.html".to_string()
    } else {
        path_str.to_string()
    };

    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;

    match site::SiteInfo::from_request(host, &client, &site_map, &site_alias_map).await {
        Ok(Some(site)) => {
            match site.serve_route(&format!("/{}", path_buf)).await {
                Ok(file_path) => {
                    let mut file = File::open(&file_path).await.map_err(|_| {
                        error!("Failed to open file");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;

                    let mut contents = Vec::new();
                    file.read_to_end(&mut contents).await.map_err(|_| {
                        error!("Failed to read file");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;

                    let mut response = Response::new(axum::body::Body::from(contents));
                    let content_type = match file_path.extension().and_then(|e| e.to_str()) {
                        Some("html") | Some("htm") => "text/html",
                        Some("css") => "text/css",
                        Some("js") => "application/javascript",
                        Some("json") => "application/json",
                        Some("png") => "image/png",
                        Some("jpg") | Some("jpeg") => "image/jpeg",
                        Some("gif") => "image/gif",
                        Some("svg") => "image/svg+xml",
                        Some("woff") => "font/woff",
                        Some("woff2") => "font/woff2",
                        _ => "application/octet-stream",
                    };
                    response
                        .headers_mut()
                        .insert(header::CONTENT_TYPE, content_type.parse().unwrap());
                    Ok(response)
                }
                Err(e) => {
                    error!("Failed to serve route: {}", e);
                    Err(StatusCode::NOT_FOUND)
                }
            }
        }
        Ok(None) => {
            // No subdomain - serve index.html
            let mut response = Response::new(axum::body::Body::from(INDEX_HTML));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, "text/html".parse().unwrap());
            Ok(response)
        }
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}
