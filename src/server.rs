#![warn(unreachable_pub)]

use std::net::SocketAddr;

use axum::{
    routing::{get, post},
    Router, Server,
};
use sqlx::postgres::PgConnectOptions;
use tower_http::{
    cors::CorsLayer,
    trace::{DefaultOnRequest, DefaultOnResponse, TraceLayer},
    LatencyUnit,
};
use tracing::Level;

mod handlers;
mod metrics;
mod slug;

#[tracing::instrument(level = "trace")]
pub(crate) async fn run(addr: &SocketAddr, opts: PgConnectOptions) -> color_eyre::Result<()> {
    let shared = handlers::Shared::default_settings(opts).await?;
    let routes = Router::new()
        .route("/:slug", get(handlers::resolve))
        .route("/rev/:url", get(handlers::reverse))
        .route("/reg", post(handlers::register))
        .nest(
            "/admin",
            Router::new().route("/metrics", get(handlers::admin_metrics)),
        )
        .with_state(shared)
        .layer(CorsLayer::permissive())
        .layer(
            TraceLayer::new_for_http()
                .on_request(DefaultOnRequest::new().level(Level::TRACE))
                .on_response(
                    DefaultOnResponse::new()
                        .level(Level::TRACE)
                        .latency_unit(LatencyUnit::Micros),
                ),
        )
        .into_make_service();

    tracing::info!(address = %addr, "starting server");
    let result = Server::bind(addr).serve(routes).await;

    if let Err(e) = result {
        tracing::error!(cause = %e, "server error");
    }

    Ok(())
}

pub(crate) fn setup_tracing(level: tracing::Level) {
    use std::io::{self, IsTerminal};
    use tracing_subscriber::{
        filter,
        fmt::{self, time::UtcTime},
        prelude::*,
        registry,
    };

    let filter = filter::Targets::new()
        .with_target(env!("CARGO_PKG_NAME"), level)
        .with_target("tower_http::trace", tracing::Level::TRACE);
    let fmt = fmt::layer()
        .with_target(false)
        .with_timer(UtcTime::rfc_3339());

    if io::stdout().is_terminal() {
        registry().with(fmt.pretty()).with(filter).init();
    } else {
        registry()
            .with(
                fmt.json()
                    .flatten_event(true)
                    .with_file(false)
                    .with_line_number(false)
                    .with_current_span(false)
                    .with_span_list(true),
            )
            .with(filter)
            .init();
    }
}
