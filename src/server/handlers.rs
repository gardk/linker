use std::sync::Arc;

use axum::{
    debug_handler,
    extract::{Host, Path, State},
    http::StatusCode,
    response::Redirect,
};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};
use tracing::instrument;
use url::Url;

use crate::server::metrics::Labels;

use super::{metrics::Metrics, slug::Slug};

type Cache = moka::sync::Cache<Slug, Arc<str>, ahash::RandomState>;

// Shared state required by all handlers.
#[derive(Clone)]
pub(super) struct Shared {
    pool: PgPool,
    cache: Cache,
    metrics: Metrics,
}

impl Shared {
    pub(super) async fn default_settings(opts: PgConnectOptions) -> color_eyre::Result<Self> {
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(10)
            .connect_with(opts)
            .await?;
        let cache = moka::sync::Cache::builder()
            .max_capacity(1000)
            .build_with_hasher(ahash::RandomState::new());
        Ok(Self {
            pool,
            cache,
            metrics: Metrics::default(),
        })
    }
}

#[instrument(skip_all, fields(%slug))]
#[debug_handler]
pub(super) async fn resolve(
    State(Shared {
        pool,
        cache,
        metrics,
    }): State<Shared>,
    Path(slug): Path<Slug>,
) -> Result<Redirect, StatusCode> {
    // All requests are counted no matter their outcome
    let labels = Labels {
        handler: "resolve",
        slug: Some(slug),
    };
    metrics.http_requests.get_or_create(&labels).inc();

    // Fast-path cache hits
    if let Some(url) = cache.get(&slug) {
        metrics.cache_hits.get_or_create(&labels).inc();
        return Ok(Redirect::permanent(&url));
    }
    metrics.cache_misses.get_or_create(&labels).inc();

    let url = sqlx::query_scalar!("SELECT url FROM links WHERE slug = $1", slug.as_str())
        .fetch_optional(&pool)
        .await;

    match url {
        Ok(Some(url)) => {
            let resp = Redirect::permanent(&url);
            cache.insert(slug, url.into());
            Ok(resp)
        }
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!(cause = %e, "unable to resolve slug");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

#[instrument(skip_all)]
#[debug_handler]
pub(super) async fn reverse(
    State(Shared { pool, metrics, .. }): State<Shared>,
    Path(url): Path<Url>,
) -> Result<String, StatusCode> {
    let slug = sqlx::query_scalar!("SELECT slug FROM links WHERE url = $1", url.as_str())
        .fetch_optional(&pool)
        .await;

    match slug {
        Ok(Some(slug)) => {
            metrics
                .http_requests
                .get_or_create(&Labels {
                    handler: "reverse",
                    // Slugs should always be correct length.
                    slug: Some(Slug::try_from(slug.as_str()).unwrap()),
                })
                .inc();
            Ok(slug)
        }
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!(cause = %e, "unable to reverse lookup");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

#[instrument(skip_all)]
#[debug_handler]
pub(super) async fn register(
    State(Shared { pool, cache, .. }): State<Shared>,
    Host(host): Host,
    url: String,
) -> Result<String, StatusCode> {
    let Ok(url) = Url::parse(&url) else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let mut retries = 0;

    loop {
        let slug = Slug::from_rng(&mut rand::thread_rng());

        let result = sqlx::query!(
            "INSERT INTO links (slug, url) VALUES ($1, $2)",
            slug.as_str(),
            url.as_str(),
        )
        .execute(&pool)
        .await;

        break match result {
            Ok(_) => {
                tracing::debug!(%slug, "created");
                cache.insert(slug, String::from(url).into());
                // There is probably a better way to do this, but I can't be asked.
                Ok(format!("http://{host}/{slug}"))
            }
            Err(e) => Err(match e.as_database_error().and_then(|e| e.constraint()) {
                Some("links_pkey") if retries < 2 => {
                    tracing::debug!(%retries, "retrying");
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

#[instrument(skip_all)]
#[debug_handler]
pub(super) async fn admin_metrics(
    State(Shared { metrics, .. }): State<Shared>,
) -> Result<String, StatusCode> {
    let mut buffer = String::with_capacity(4096);
    let res = prometheus_client::encoding::text::encode(&mut buffer, &metrics);
    match res {
        Ok(()) => Ok(buffer),
        Err(e) => {
            tracing::error!(cause = %e, "unable to encode metrics");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
