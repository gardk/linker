use std::sync::Arc;

use anyhow::Context;
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};

use crate::{metrics::Registry, slug::Slug};

use self::states::*;

pub(super) type Cache = moka::sync::Cache<Slug, Arc<str>, ahash::RandomState>;

#[derive(Clone)]
pub struct Shared {
    pub(super) pool: PgPool,
    pub(super) cache: Cache,
    pub(super) registry: Registry,
}

impl Shared {
    #[inline]
    pub fn builder() -> Builder<Init> {
        Builder::default()
    }
}

pub struct Builder<S> {
    state: S,
}

impl Default for Builder<Init> {
    fn default() -> Self {
        Builder { state: Init }
    }
}

impl Builder<Init> {
    #[inline]
    pub fn with_connect_opts(self, opts: PgConnectOptions) -> Builder<HasPool> {
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(2)
            .connect_with(opts);
        Builder {
            state: HasPool {
                pool: Box::pin(pool),
            },
        }
    }
}

impl Builder<HasPool> {
    #[inline]
    pub fn with_max_cache_capacity(self, capacity: u64) -> Builder<HasCache> {
        let cache = moka::sync::Cache::builder()
            .max_capacity(capacity)
            .build_with_hasher(ahash::RandomState::new());
        Builder {
            state: HasCache {
                pool: self.state.pool,
                cache,
            },
        }
    }
}

impl Builder<HasCache> {
    pub async fn build(self) -> anyhow::Result<Shared> {
        let pool = self
            .state
            .pool
            .await
            .context("unable to establish database connection")?;
        sqlx::migrate!()
            .run(&pool)
            .await
            .context("unable to run database migrations")?;
        Ok(Shared {
            pool,
            cache: self.state.cache,
            registry: Registry::default(),
        })
    }
}

mod states {
    use std::future::Future;
    use std::pin::Pin;

    use super::*;

    #[doc(hidden)]
    pub struct Init;

    #[doc(hidden)]
    pub struct HasPool {
        pub(super) pool: Pin<Box<dyn Future<Output = sqlx::Result<PgPool>>>>,
    }

    #[doc(hidden)]
    pub struct HasCache {
        pub(super) pool: Pin<Box<dyn Future<Output = sqlx::Result<PgPool>>>>,
        pub(super) cache: Cache,
    }
}
