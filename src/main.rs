#![warn(unused_crate_dependencies)]

use std::env;

use color_eyre::eyre::Context;
use mimalloc::MiMalloc;
use sqlx::{postgres::PgConnectOptions, ConnectOptions};
use tracing::Level;
use url::Url;

mod server;

// Marginal performance improvement.
#[global_allocator]
static ALLOC: MiMalloc = MiMalloc;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    server::setup_tracing(Level::DEBUG);

    let addr = env::var("LISTEN_ADDR")
        .wrap_err("unable to read LISTEN_ADDR")
        .and_then(|s| {
            tracing::debug!(listen_addr = %s);
            s.parse().map_err(Into::into)
        })?;
    let conn_opts = env::var("DATABASE_URL")
        .wrap_err("unable to read DATABASE_URL")
        .and_then(|s| {
            let mut url = Url::parse(&s)?;
            let conn_opts = PgConnectOptions::from_url(&url)?;
            if tracing::enabled!(Level::DEBUG) {
                url.set_password(None).unwrap();
                tracing::debug!(database_url = %url);
            }
            Ok(conn_opts)
        })?;

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(server::run(&addr, conn_opts))
}
