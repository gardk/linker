use std::{env, net::SocketAddr};

use anyhow::Context;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Redirect,
    routing::{get, post},
    Router, Server,
};
use moka::sync::Cache;
use rand::distributions::{Alphanumeric, DistString};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};
use tower_http::cors::CorsLayer;
use url::Url;

type Slug = arrayvec::ArrayString<10>;

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
    let pool = PgPoolOptions::new()
        .min_connections(1)
        .max_connections(3)
        .connect_with(conn_opts)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    let cache: Cache<Slug, String, _> = Cache::builder()
        .max_capacity(1000)
        .build_with_hasher(ahash::RandomState::new());
    let routes = Router::new()
        .route("/:slug", get(resolve))
        .route("/rev/:url", get(reverse))
        .route("/post/:url", post(generate))
        .with_state(Shared { pool, cache })
        .layer(CorsLayer::permissive())
        .into_make_service();

    tracing::info!(%addr, "starting server");
    let result = Server::bind(addr).serve(routes).await;

    if let Err(e) = result {
        tracing::error!(cause = %e, "server error");
    }

    Ok(())
}

#[derive(Clone)]
struct Shared {
    pool: PgPool,
    cache: Cache<Slug, String, ahash::RandomState>,
}

#[tracing::instrument(skip(pool, cache))]
async fn resolve(
    State(Shared { pool, cache }): State<Shared>,
    Path(slug): Path<Slug>,
) -> Result<Redirect, StatusCode> {
    if let Some(url) = cache.get(&slug) {
        return Ok(Redirect::permanent(url.as_str()));
    }

    let url = sqlx::query_scalar!("SELECT url FROM links WHERE slug = $1", slug.as_str())
        .fetch_optional(&pool)
        .await;

    match url {
        Ok(Some(url)) => {
            let response = Redirect::permanent(&url);
            cache.insert(slug, url);
            Ok(response)
        }
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!(cause = %e, "unable to resolve slug");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

#[tracing::instrument(skip(pool))]
async fn reverse(
    State(Shared { pool, .. }): State<Shared>,
    Path(url): Path<Url>,
) -> Result<String, StatusCode> {
    let slug = sqlx::query_scalar!("SELECT slug FROM links WHERE url = $1", url.as_str())
        .fetch_optional(&pool)
        .await;

    match slug {
        Ok(Some(slug)) => Ok(slug),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!(cause = %e, "unable to reverse slug");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

#[tracing::instrument]
async fn generate(
    State(Shared { pool, cache }): State<Shared>,
    Path(url): Path<Url>,
) -> Result<String, StatusCode> {
    let Ok(mut tx) = pool.begin().await else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let mut retries = 0;

    loop {
        let slug = Alphanumeric.sample_string(&mut rand::thread_rng(), 10);
        let result = sqlx::query!(
            "INSERT INTO links (slug, url) VALUES ($1, $2) ON CONFLICT (url) DO NOTHING",
            &slug,
            url.as_str()
        )
        .execute(&mut *tx)
        .await
        .map(|q| q.rows_affected());

        break match result {
            Ok(1) => {
                tx.commit().await.unwrap();
                cache.insert(Slug::from(&slug).unwrap(), url.into());
                Ok(slug)
            }
            Ok(0) => Err(StatusCode::CONFLICT),
            Err(e) if retries < 3 && is_pk_conflict(&e) => {
                retries += 1;
                continue;
            }
            Err(e) => {
                tracing::error!(cause = %e, "unable to generate link");
                Err(StatusCode::SERVICE_UNAVAILABLE)
            }
            Ok(_) => unreachable!(),
        };
    }
}

fn is_pk_conflict(e: &sqlx::Error) -> bool {
    e.as_database_error().and_then(|e| e.constraint()) == Some("links_pkey")
}
