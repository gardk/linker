use std::{env, net::SocketAddr, sync::Arc};

use anyhow::Context;
use axum::{
    extract::{Host, Path, State},
    http::StatusCode,
    response::Redirect,
    routing::{get, post},
    Router, Server,
};
use mimalloc::MiMalloc;
use moka::sync::Cache;
use prometheus_client::{
    encoding::{self, EncodeLabelSet, EncodeLabelValue},
    metrics::{counter::Counter, family::Family},
    registry::Registry,
};
use rand::distributions::{Alphanumeric, DistString};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};
use tower_http::cors::CorsLayer;
use url::Url;

type Slug = arrayvec::ArrayString<10>;

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

async fn entrypoint(addr: &SocketAddr, conn_opts: PgConnectOptions) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .min_connections(1)
        .max_connections(3)
        .connect_with(conn_opts)
        .await?;
    sqlx::migrate!().run(&pool).await?;

    let mut registry = Registry::default();
    let http_requests = Family::<Labels, Counter>::default();
    registry.register(
        "linker_http_requests",
        "Number of HTTP requests received",
        http_requests.clone(),
    );
    let registry = Arc::new(registry);

    let cache = Cache::builder()
        .max_capacity(1000)
        .build_with_hasher(ahash::RandomState::new());
    let routes = Router::new()
        .route("/:slug", get(resolve))
        .route("/rev/:url", get(reverse))
        .route("/post/:url", post(generate))
        .route("/metrics", get(metrics))
        .with_state(Shared {
            pool,
            cache,
            registry,
            http_requests,
        })
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
    registry: Arc<Registry>,
    http_requests: Family<Labels, Counter>,
}

#[tracing::instrument(skip_all, fields(%slug))]
async fn resolve(
    State(Shared {
        pool,
        cache,
        http_requests,
        ..
    }): State<Shared>,
    Path(slug): Path<Slug>,
) -> Result<Redirect, StatusCode> {
    http_requests
        .get_or_create(&Labels {
            handler: "resolve",
            slug: SlugLabelValue(slug),
        })
        .inc();
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

#[tracing::instrument(skip_all)]
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

#[tracing::instrument(skip_all)]
async fn generate(
    State(Shared { pool, cache, .. }): State<Shared>,
    Host(host): Host,
    Path(url): Path<Url>,
) -> Result<String, StatusCode> {
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!(cause = %e, "unable to start transaction for generate");
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    };
    let mut retries = 0;

    loop {
        let slug = Slug::from(&Alphanumeric.sample_string(&mut rand::thread_rng(), 10)).unwrap();
        let result = sqlx::query!(
            "INSERT INTO links (slug, url) VALUES ($1, $2)",
            slug.as_str(),
            url.as_str()
        )
        .execute(&mut *tx)
        .await;

        break match result {
            Ok(_) => {
                if let Err(e) = tx.commit().await {
                    tracing::error!(cause = %e, "unable to commit insert");
                    return Err(StatusCode::INTERNAL_SERVER_ERROR);
                }
                cache.insert(slug, url.into());
                Ok(format!("https://{host}/{slug}"))
            }
            Err(e) => Err(match e.as_database_error().and_then(|e| e.constraint()) {
                Some("links_pkey") if retries < 3 => {
                    retries += 1;
                    continue;
                }
                Some("links_url_key") => StatusCode::CONFLICT,
                _ => {
                    tracing::error!(cause = %e, "unable to generate link");
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }),
        };
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, EncodeLabelSet)]
struct Labels {
    handler: &'static str,
    slug: SlugLabelValue,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct SlugLabelValue(Slug);

impl EncodeLabelValue for SlugLabelValue {
    fn encode(&self, encoder: &mut encoding::LabelValueEncoder<'_>) -> Result<(), std::fmt::Error> {
        <&str as EncodeLabelValue>::encode(&self.0.as_str(), encoder)
    }
}

#[tracing::instrument(skip_all)]
async fn metrics(State(Shared { registry, .. }): State<Shared>) -> Result<String, StatusCode> {
    let mut buffer = String::with_capacity(4096);
    if let Err(e) = encoding::text::encode(&mut buffer, &registry) {
        tracing::error!(cause = %e, "unable to encode metrics");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(buffer)
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
