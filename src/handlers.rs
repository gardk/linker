use axum::{
    extract::{Host, Path, State},
    http::StatusCode,
    response::Redirect,
};
use prometheus_client::encoding;
use url::Url;

use crate::{metrics::Labels, slug::Slug};

pub mod shared;

pub use self::shared::Shared;

#[tracing::instrument(skip_all, fields(%slug))]
pub async fn resolve(
    State(Shared {
        pool,
        cache,
        registry,
    }): State<Shared>,
    Path(slug): Path<Slug>,
) -> Result<Redirect, StatusCode> {
    // All requests are counted no matter their outcome
    let labels = Labels {
        handler: "resolve",
        slug: Some(slug),
    };
    registry.http_requests.get_or_create(&labels).inc();

    // Fast-path cache hits
    if let Some(url) = cache.get(&slug) {
        registry.cache_hits.get_or_create(&labels).inc();
        return Ok(Redirect::permanent(&url));
    }
    registry.cache_misses.get_or_create(&labels).inc();

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

#[tracing::instrument(skip_all)]
pub async fn reverse(
    State(Shared { pool, registry, .. }): State<Shared>,
    Path(url): Path<Url>,
) -> Result<String, StatusCode> {
    let slug = sqlx::query_scalar!("SELECT slug FROM links WHERE url = $1", url.as_str())
        .fetch_optional(&pool)
        .await;

    match slug {
        Ok(Some(slug)) => {
            registry
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

#[tracing::instrument(skip_all)]
pub async fn generate(
    State(Shared { pool, cache, .. }): State<Shared>,
    Host(host): Host,
    Path(url): Path<Url>,
) -> Result<String, StatusCode> {
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

#[tracing::instrument(skip_all)]
pub async fn metrics(State(Shared { registry, .. }): State<Shared>) -> Result<String, StatusCode> {
    let mut buffer = String::with_capacity(4096);
    if let Err(e) = encoding::text::encode(&mut buffer, &registry) {
        tracing::error!(cause = %e, "unable to encode metrics");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(buffer)
}
