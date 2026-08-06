//! Provider-isolated Meta Model API discovery.

use crate::agent::config::{EnvKeys, ModelEntry, ModelInfo};
use anyhow::{Context, anyhow};
use indexmap::IndexMap;
use serde::Deserialize;
use std::num::NonZeroU64;
use std::time::Duration;
use url::Url;
use xai_grok_sampling_types::{
    ApiBackend, ModelProvider, ReasoningEffort, ReasoningEffortOption, ToolMode,
};

pub const META_API_BASE_URL: &str = "https://api.meta.ai/v1";
pub const META_API_BASE_URL_ENV: &str = "OPENGROK_META_API_BASE_URL";
pub const META_API_KEY_ENV: &str = "META_API_KEY";
const META_MODELS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
pub struct CuratedMetaModel {
    pub key: &'static str,
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

pub const CURATED_META_MODELS: [CuratedMetaModel; 3] = [
    CuratedMetaModel {
        key: "meta:muse-spark-1.2",
        slug: "muse-spark-1.2",
        name: "Muse Spark 1.2",
        description: "Meta's Muse Spark 1.2 agentic reasoning model",
    },
    CuratedMetaModel {
        key: "meta:muse-spark-1.1",
        slug: "muse-spark-1.1",
        name: "Muse Spark 1.1",
        description: "Meta's Muse Spark 1.1 multimodal reasoning model",
    },
    CuratedMetaModel {
        key: "meta:muse-spark-1.2-contributor",
        slug: "muse-spark-1.2-contributor",
        name: "Muse Spark 1.2 Contributor",
        description: "Meta's contributor-tuned Muse Spark 1.2 model",
    },
];

pub fn is_trusted_api_base_url(base_url: &str) -> bool {
    let Ok(url) = Url::parse(base_url) else {
        return false;
    };
    url.scheme() == "https" && url.host_str() == Some("api.meta.ai")
}

pub fn api_base_url() -> String {
    std::env::var(META_API_BASE_URL_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| META_API_BASE_URL.to_owned())
}

fn environment_api_key() -> Option<String> {
    std::env::var(META_API_KEY_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub fn environment_api_key_is_configured() -> bool {
    environment_api_key().is_some()
}

fn stored_api_key() -> Option<String> {
    crate::auth::read_provider_api_key(&crate::util::grok_home::grok_home(), ModelProvider::Meta)
}

fn select_api_key(
    base_url: &str,
    environment_key: Option<String>,
    stored_key: Option<String>,
) -> Option<String> {
    environment_key.or_else(|| {
        is_trusted_api_base_url(base_url)
            .then_some(stored_key)
            .flatten()
    })
}

fn api_key_for_base_url(base_url: &str) -> Option<String> {
    select_api_key(base_url, environment_api_key(), stored_api_key())
}

fn credential_fingerprint(api_key: &str) -> String {
    blake3::hash(api_key.as_bytes()).to_hex().to_string()
}

fn curated_model_entry(curated: &CuratedMetaModel, base_url: &str) -> ModelEntry {
    let mut info = ModelInfo::fallback(curated.key);
    info.id = Some(curated.key.to_owned());
    info.model = curated.slug.to_owned();
    info.base_url = base_url.trim_end_matches('/').to_owned();
    info.name = Some(curated.name.to_owned());
    info.description = Some(curated.description.to_owned());
    info.api_backend = ApiBackend::Responses;
    info.provider = ModelProvider::Meta;
    info.tool_mode = Some(ToolMode::Direct);
    info.context_window = NonZeroU64::new(1_000_000).expect("non-zero Meta context window");
    info.supports_reasoning_effort = true;
    info.reasoning_efforts = meta_reasoning_efforts();
    info.reasoning_effort = Some(ReasoningEffort::Medium);
    info.supports_backend_search = true;
    info.supported_in_api = true;
    ModelEntry {
        info,
        api_key: None,
        env_key: Some(EnvKeys::single(META_API_KEY_ENV)),
        auth_provider: None,
        api_base_url: None,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MetaModelsCatalog {
    entries: IndexMap<String, ModelEntry>,
    credential_fingerprint: String,
}

impl MetaModelsCatalog {
    pub(crate) fn entries(&self) -> IndexMap<String, ModelEntry> {
        self.entries.clone()
    }

    pub(crate) fn is_authoritative(&self) -> bool {
        !self.entries.is_empty()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MetaModelsClient {
    http: reqwest::Client,
    base_url: String,
}

impl MetaModelsClient {
    pub(crate) fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: api_base_url(),
        }
    }

    #[cfg(test)]
    fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }

    pub(crate) async fn query(&self) -> anyhow::Result<Option<MetaModelsCatalog>> {
        let Some(api_key) = api_key_for_base_url(&self.base_url) else {
            return Ok(None);
        };
        self.query_with_key(&api_key).await.map(Some)
    }

    pub(crate) fn has_usable_api_key(&self) -> bool {
        api_key_for_base_url(&self.base_url).is_some()
    }

    pub(crate) fn catalog_matches_current_credential(&self, catalog: &MetaModelsCatalog) -> bool {
        api_key_for_base_url(&self.base_url)
            .map(|key| credential_fingerprint(&key))
            .is_some_and(|fingerprint| fingerprint == catalog.credential_fingerprint)
    }

    async fn query_with_key(&self, api_key: &str) -> anyhow::Result<MetaModelsCatalog> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let response = self
            .http
            .get(&url)
            .timeout(META_MODELS_REQUEST_TIMEOUT)
            .bearer_auth(api_key)
            .send()
            .await
            .with_context(|| format!("Meta models request to {url} failed"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Meta models request returned {status}: {}",
                safe_error_excerpt(&body, api_key)
            ));
        }
        let wire: MetaModelsResponse = response
            .json()
            .await
            .context("Meta models response was invalid")?;
        Ok(self.catalog_from_wire(wire, api_key))
    }

    fn catalog_from_wire(&self, wire: MetaModelsResponse, api_key: &str) -> MetaModelsCatalog {
        let available = wire
            .data
            .into_iter()
            .map(|model| model.id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .collect::<std::collections::HashSet<_>>();
        let entries = CURATED_META_MODELS
            .iter()
            .filter(|curated| available.contains(curated.slug))
            .map(|curated| {
                (
                    curated.key.to_owned(),
                    curated_model_entry(curated, &self.base_url),
                )
            })
            .collect();
        MetaModelsCatalog {
            entries,
            credential_fingerprint: credential_fingerprint(api_key),
        }
    }
}

fn meta_reasoning_efforts() -> Vec<ReasoningEffortOption> {
    [
        (
            ReasoningEffort::Low,
            "Faster responses with lighter reasoning",
            false,
        ),
        (
            ReasoningEffort::Medium,
            "Balanced reasoning depth for everyday tasks",
            true,
        ),
        (
            ReasoningEffort::High,
            "Greater reasoning depth for complex problems",
            false,
        ),
        (
            ReasoningEffort::Xhigh,
            "Extra-high reasoning depth for difficult tasks",
            false,
        ),
    ]
    .into_iter()
    .map(|(value, description, default)| ReasoningEffortOption {
        id: value.as_str().to_owned(),
        value,
        label: match value {
            ReasoningEffort::Low => "Low",
            ReasoningEffort::Medium => "Medium",
            ReasoningEffort::High => "High",
            ReasoningEffort::Xhigh => "XHigh",
            _ => unreachable!("Meta exposes only low/medium/high/xhigh"),
        }
        .to_owned(),
        description: Some(description.to_owned()),
        default,
    })
    .collect()
}

fn safe_error_excerpt(body: &str, api_key: &str) -> String {
    let sanitized = body
        .replace(api_key, "[REDACTED]")
        .replace(['\r', '\n'], " ");
    sanitized.chars().take(512).collect()
}

#[derive(Debug, Deserialize)]
struct MetaModelsResponse {
    #[serde(default)]
    data: Vec<MetaWireModel>,
}

#[derive(Debug, Deserialize)]
struct MetaWireModel {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::get};
    use std::sync::{Arc, Mutex};

    #[test]
    fn trusted_hosts_are_provider_scoped() {
        assert!(is_trusted_api_base_url(META_API_BASE_URL));
        assert!(is_trusted_api_base_url("https://api.meta.ai/v1"));
        assert!(!is_trusted_api_base_url("http://api.meta.ai/v1"));
        assert!(!is_trusted_api_base_url("https://api.x.ai/v1"));
        assert!(!is_trusted_api_base_url("https://meta.example/v1"));
    }

    #[test]
    fn stored_keys_never_leave_owned_hosts() {
        let stored = Some("meta-stored-secret".to_owned());
        assert_eq!(
            select_api_key(META_API_BASE_URL, None, stored.clone()).as_deref(),
            Some("meta-stored-secret")
        );
        assert_eq!(
            select_api_key("https://proxy.example/v1", None, stored),
            None
        );
        assert_eq!(
            select_api_key(
                "https://proxy.example/v1",
                Some("explicit-environment-secret".to_owned()),
                None,
            )
            .as_deref(),
            Some("explicit-environment-secret")
        );
    }

    #[test]
    fn wire_catalog_preserves_curated_capabilities() {
        let client = MetaModelsClient::with_base_url(META_API_BASE_URL);
        let catalog = client.catalog_from_wire(
            MetaModelsResponse {
                data: vec![
                    MetaWireModel {
                        id: "muse-spark-1.2".to_owned(),
                    },
                    MetaWireModel {
                        id: "muse-spark-1.1".to_owned(),
                    },
                    MetaWireModel {
                        id: "muse-spark-1.2-contributor".to_owned(),
                    },
                    MetaWireModel {
                        id: "future-unknown".to_owned(),
                    },
                ],
            },
            "catalog-key",
        );
        let entries = catalog.entries();
        assert_eq!(entries.len(), 3);
        let model = &entries["meta:muse-spark-1.2"];
        assert_eq!(model.info.provider, ModelProvider::Meta);
        assert_eq!(model.info.api_backend, ApiBackend::Responses);
        assert_eq!(model.info.context_window.get(), 1_000_000);
        assert!(model.info.supports_backend_search);
        assert_eq!(model.info.reasoning_effort, Some(ReasoningEffort::Medium));
        assert_eq!(
            model
                .info
                .reasoning_efforts
                .iter()
                .map(|option| option.value)
                .collect::<Vec<_>>(),
            vec![
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::Xhigh
            ]
        );
        assert_eq!(
            model.env_key.as_ref().and_then(EnvKeys::primary),
            Some(META_API_KEY_ENV)
        );
    }

    #[test]
    fn error_excerpt_redacts_a_reflected_credential() {
        let excerpt =
            safe_error_excerpt("request rejected for meta-canary\ntry again", "meta-canary");
        assert_eq!(excerpt, "request rejected for [REDACTED] try again");
    }

    #[tokio::test]
    async fn model_query_uses_bearer_auth() {
        #[derive(Clone, Default)]
        struct RequestCapture(Arc<Mutex<Option<String>>>);

        async fn models(
            State(capture): State<RequestCapture>,
            headers: HeaderMap,
        ) -> Json<serde_json::Value> {
            *capture.0.lock().expect("capture lock") = headers
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            Json(serde_json::json!({
                "object": "list",
                "data": [{"id": "muse-spark-1.2"}]
            }))
        }

        let capture = RequestCapture::default();
        let app = Router::new()
            .route("/models", get(models))
            .with_state(capture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = MetaModelsClient::with_base_url(format!("http://{address}"));
        let catalog = client.query_with_key("meta-query-canary").await.unwrap();
        assert!(catalog.entries().contains_key("meta:muse-spark-1.2"));
        assert_eq!(
            capture.0.lock().unwrap().as_deref(),
            Some("Bearer meta-query-canary")
        );
    }
}
