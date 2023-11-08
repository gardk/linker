use std::{env, net::SocketAddr};

use anyhow::Context;
use axum::{
    routing::{get, post},
    Router, Server,
};
use linker::handlers;
use mimalloc::MiMalloc;
use sqlx::postgres::PgConnectOptions;
use tower_http::cors::CorsLayer;

#[global_allocator]
static ALLOC: MiMalloc = MiMalloc;

fn main() -> anyhow::Result<()> {
    let addr = env::var("LISTEN_ADDR")
        .context("unable to read LISTEN_ADDR")
        .and_then(|s| s.parse().map_err(Into::into))?;
    let conn_opts = env::var("DATABASE_URL")
        .context("unable to read DATABASE_URL")
        .and_then(|s| s.parse().map_err(Into::into))?;

    tracing_setup();

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(entrypoint(&addr, conn_opts))
}

fn tracing_setup() {
    use tracing_subscriber::{filter, fmt, prelude::*};

    let filter = filter::Targets::new().with_target(env!("CARGO_PKG_NAME"), tracing::Level::DEBUG);
    let fmt = fmt::layer()
        .json()
        .flatten_event(true)
        .with_file(false)
        .with_line_number(false)
        .with_target(false)
        .with_current_span(false)
        .with_span_list(true)
        .with_timer(fmt::time::UtcTime::rfc_3339());

    tracing_subscriber::registry().with(fmt).with(filter).init();
}

async fn entrypoint(addr: &SocketAddr, conn_opts: PgConnectOptions) -> anyhow::Result<()> {
    let shared = handlers::Shared::builder()
        .with_connect_opts(conn_opts)
        .with_max_cache_capacity(1000)
        .build()
        .await?;
    let routes = Router::new()
        .route("/:slug", get(handlers::resolve))
        .route("/rev/:url", get(handlers::reverse))
        .route("/post/:url", post(handlers::generate))
        .route("/metrics", get(handlers::metrics))
        .with_state(shared)
        .layer(CorsLayer::permissive())
        .into_make_service();

    tracing::info!(%addr, "starting server");
    let result = Server::bind(addr).serve(routes).await;

    if let Err(e) = result {
        tracing::error!(cause = %e, "server error");
    }

    Ok(())
}
