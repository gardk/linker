use std::{ops::Deref, sync::Arc};

use prometheus_client::{
    encoding::{EncodeLabelSet, EncodeLabelValue, LabelValueEncoder},
    metrics::{counter::Counter, family::Family},
    registry,
};

use crate::slug::Slug;

#[derive(Clone)]
pub struct Registry {
    registry: Arc<registry::Registry>,
    // Metric families
    pub http_requests: Family<Labels, Counter>,
    pub cache_hits: Family<Labels, Counter>,
    pub cache_misses: Family<Labels, Counter>,
}

impl Default for Registry {
    fn default() -> Self {
        let mut registry = registry::Registry::default();
        let http_requests = Family::<Labels, Counter>::default();
        let cache_hits = Family::<Labels, Counter>::default();
        let cache_misses = Family::<Labels, Counter>::default();
        registry.register(
            "linker_http_requests",
            "Number of handled HTTP requests",
            http_requests.clone(),
        );
        registry.register(
            "linker_cache_hits",
            "Amount of cache hits",
            cache_hits.clone(),
        );
        registry.register(
            "linker_cache_misses",
            "Amount of cache misses",
            cache_misses.clone(),
        );
        let registry = Arc::new(registry);

        Self {
            registry,
            http_requests,
            cache_hits,
            cache_misses,
        }
    }
}

impl Deref for Registry {
    type Target = registry::Registry;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.registry
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, EncodeLabelSet)]
pub struct Labels {
    pub handler: &'static str,
    pub slug: Option<Slug>,
}

impl EncodeLabelValue for Slug {
    fn encode(&self, encoder: &mut LabelValueEncoder<'_>) -> Result<(), std::fmt::Error> {
        self.as_str().encode(encoder)
    }
}
