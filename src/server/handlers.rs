use std::sync::Arc;

use axum::{
    body::StreamBody,
    debug_handler,
    extract::{Form, Host, Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
use futures_util::{Stream, TryStreamExt};
use serde::{Deserialize, Serialize};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};
use tracing::instrument;
use url::Url;

use crate::server::metrics::Labels;

use super::{metrics::Metrics, slug::Slug};

type Cache = moka::sync::Cache<Slug, (Arc<str>, bool), ahash::RandomState>;

// Shared state required by all handlers.
pub(super) struct Shared {
    pool: PgPool,
    cache: Cache,
    metrics: Metrics,
}

impl Shared {
    pub(super) async fn default_settings(
        opts: PgConnectOptions,
    ) -> color_eyre::Result<&'static Self> {
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(10)
            .connect_with(opts)
            .await?;
        let cache = moka::sync::Cache::builder()
            .max_capacity(1000)
            .build_with_hasher(ahash::RandomState::new());
        Ok(Box::leak(Box::new(Self {
            pool,
            cache,
            metrics: Metrics::default(),
        })))
    }
}

#[instrument(skip_all, fields(%slug))]
#[debug_handler]
pub(super) async fn resolve(
    State(Shared {
        pool,
        cache,
        metrics,
    }): State<&'static Shared>,
    Path(slug): Path<Slug>,
) -> Result<Response, StatusCode> {
    // All requests are counted no matter their outcome
    metrics
        .http_requests
        .get_or_create(&Labels {
            handler: "resolve",
            slug: Some(slug),
        })
        .inc();

    // Fast-path cache hits
    if let Some((url, hidden)) = cache.get(&slug) {
        return Ok(create_redirect(&url, hidden));
    }
    metrics.cache_misses.inc();

    let row = sqlx::query!(
        "SELECT url, hidden FROM links WHERE slug = $1",
        slug.as_str()
    )
    .fetch_optional(pool)
    .await;

    match row {
        Ok(Some(row)) => {
            let resp = create_redirect(&row.url, row.hidden);
            cache.insert(slug, (row.url.into(), row.hidden));
            Ok(resp)
        }
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!(cause = %e, "unable to resolve slug");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

fn create_redirect(url: &str, hidden: bool) -> Response {
    if !hidden {
        Redirect::permanent(url).into_response()
    } else {
        Html(format!(
            "<html><body><script>window.location.href='{url}';</script></body></html>"
        ))
        .into_response()
    }
}

#[instrument(skip_all)]
#[debug_handler]
pub(super) async fn reverse(
    State(Shared { pool, metrics, .. }): State<&'static Shared>,
    Path(url): Path<Url>,
) -> Result<String, StatusCode> {
    let slug = sqlx::query_scalar!("SELECT slug FROM links WHERE url = $1", url.as_str())
        .fetch_optional(pool)
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

#[derive(Deserialize)]
pub(super) struct RegisterForm {
    url: String,
    #[serde(default)]
    hidden: bool,
}

#[instrument(skip_all)]
#[debug_handler]
pub(super) async fn register(
    State(Shared { pool, cache, .. }): State<&'static Shared>,
    Host(host): Host,
    Form(form): Form<RegisterForm>,
) -> Result<String, StatusCode> {
    let Ok(url) = Url::parse(&form.url) else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let mut retries = 0;

    loop {
        let slug = Slug::from_rng(&mut rand::thread_rng());

        let result = sqlx::query!(
            "INSERT INTO links (slug, url, hidden) VALUES ($1, $2, $3)",
            slug.as_str(),
            url.as_str(),
            form.hidden,
        )
        .execute(pool)
        .await;

        break match result {
            Ok(_) => {
                tracing::debug!(%slug, "created");
                cache.insert(slug, (String::from(url).into(), form.hidden));
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
    State(Shared { metrics, .. }): State<&'static Shared>,
) -> Result<String, StatusCode> {
    let mut buffer = String::with_capacity(4096);
    let res = prometheus_client::encoding::text::encode(&mut buffer, metrics);
    match res {
        Ok(()) => Ok(buffer),
        Err(e) => {
            tracing::error!(cause = %e, "unable to encode metrics");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[instrument(skip_all)]
#[debug_handler]
pub(super) async fn admin_list(
    State(Shared { pool, .. }): State<&'static Shared>,
) -> StreamBody<impl Stream<Item = sqlx::Result<String>>> {
    #[derive(Serialize)]
    struct Row<'a> {
        slug: &'a str,
        url: &'a str,
        hidden: bool,
    }

    sqlx::query!("SELECT slug, url, hidden FROM links")
        .fetch(pool)
        .map_ok(|row| {
            let mut s = serde_json::to_string(&Row {
                slug: &row.slug,
                url: &row.url,
                hidden: row.hidden,
            })
            .unwrap();
            s.push('\n');
            s
        })
        .into()
}
