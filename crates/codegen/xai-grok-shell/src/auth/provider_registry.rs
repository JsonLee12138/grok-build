//! Provider-owned API-key routing and model discovery.
//!
//! This module deliberately keeps a provider credential separate from its
//! endpoint.  In particular, callers must obtain a [`RequestProjection`] from
//! the registry instead of combining fields from arbitrary model entries.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::agent::auth_method::ProviderAuthRequiredError;

const AUTH_STORE_VERSION: u8 = 2;
const OPENAI_MODELS_URL: &str = "https://api.openai.com/v1/models";
const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// Providers with a fixed, reviewed protocol in the initial registry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Openai,
    Openrouter,
}

impl ProviderId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Openrouter => "openrouter",
        }
    }

    pub fn from_canonical_model(model_id: &str) -> Option<Self> {
        let (provider, wire_model) = model_id.split_once('/')?;
        if wire_model.is_empty() {
            return None;
        }
        match provider {
            "openai" => Some(Self::Openai),
            "openrouter" => Some(Self::Openrouter),
            _ => None,
        }
    }
}

/// A display-safe credential reference. Its `Debug` implementation never
/// renders the secret, so assertion failures and tracing cannot disclose it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let value = value.trim().to_owned();
        if value.is_empty() {
            bail!("provider API key is empty");
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper for an authenticated transport boundary.
    ///
    /// Callers must not log or persist the returned string outside the
    /// Provider Store.
    pub fn into_secret(self) -> String {
        self.0
    }

    pub fn masked(&self) -> String {
        let tail: String = self
            .0
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("****{tail}")
    }

    fn fingerprint(&self) -> String {
        blake3::hash(self.0.as_bytes()).to_hex().to_string()
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ApiKey").field(&self.masked()).finish()
    }
}

/// Provider-only credential persistence. The endpoint is intentionally absent
/// from this type and its on-disk representation.
#[derive(Debug, Clone)]
pub struct ProviderCredentialStore {
    path: PathBuf,
}

impl ProviderCredentialStore {
    pub fn new(grok_home: impl Into<PathBuf>) -> Self {
        Self {
            path: grok_home.into().join("auth.json"),
        }
    }

    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn store_api_key(&self, provider: ProviderId, key: ApiKey) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("auth.json has no parent"))?;
        if !parent.exists() {
            fs::create_dir_all(parent).context("creating auth.json directory")?;
            super::storage::set_secure_directory_permissions(parent)
                .context("securing auth.json directory")?;
        }
        let lock = super::manager::lock::try_lock_auth_file_nonblocking(&self.path)
            .ok_or_else(|| anyhow!("auth store is busy"))?;
        // Re-read only after acquiring the shared auth.json lock.  An xAI
        // writer may have updated its namespace while we waited.
        let mut root = self.read_raw()?;
        if !root.get("providers").is_some_and(Value::is_object) && self.path.exists() {
            // A legacy xAI writer may have won the race immediately before we
            // acquired the lock. Migrate those locked bytes directly; the
            // regular reader would attempt to take this non-reentrant lock.
            super::storage::migrate_legacy_store_while_locked(&self.path, &lock)
                .context("migrating locked legacy auth.json")?;
            root = self.read_raw()?;
        }
        if !root.is_object() {
            root = json!({});
        }
        root["version"] = json!(AUTH_STORE_VERSION);
        let root_object = root.as_object_mut().expect("object checked above");
        let providers = root_object
            .entry("providers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| anyhow!("auth store providers is not an object"))?;
        let provider_entry = providers
            .entry(provider.as_str().to_string())
            .or_insert_with(|| json!({}));
        let provider_object = provider_entry
            .as_object_mut()
            .ok_or_else(|| anyhow!("provider auth entry is not an object"))?;
        let credentials = provider_object
            .entry("credentials")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| anyhow!("provider credentials entry is not an object"))?;
        credentials.insert(
            "api_key".to_owned(),
            json!({ "type": "api_key", "key": key.expose() }),
        );
        if !lock.still_live(&self.path) {
            bail!("auth store lock was replaced before provider credential write");
        }
        self.write_raw(&root)
    }

    pub fn api_key(&self, provider: ProviderId) -> Result<Option<ApiKey>> {
        let root = self.read_raw()?;
        let value = root.pointer(&format!(
            "/providers/{}/credentials/api_key",
            provider.as_str()
        ));
        let Some(value) = value else {
            return Ok(None);
        };
        if value.get("type").and_then(Value::as_str) != Some("api_key") {
            return Ok(None);
        }
        value
            .get("key")
            .and_then(Value::as_str)
            .map(|s| ApiKey::new(s.to_owned()))
            .transpose()
    }

    fn read_raw(&self) -> Result<Value> {
        match fs::read_to_string(&self.path) {
            Ok(raw) if raw.trim().is_empty() => Ok(json!({ "providers": {} })),
            Ok(raw) => {
                let value: Value = serde_json::from_str(&raw).context("invalid auth.json")?;
                if value.get("providers").is_some_and(Value::is_object)
                    && value.get("version").and_then(Value::as_u64)
                        != Some(AUTH_STORE_VERSION.into())
                {
                    bail!("unsupported auth store version");
                }
                Ok(value)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(json!({ "providers": {} }))
            }
            Err(error) => Err(error).context("reading auth.json"),
        }
    }

    fn write_raw(&self, value: &Value) -> Result<()> {
        super::storage::write_provider_json_atomic(&self.path, value)
            .context("installing provider auth file")
    }
}

/// The exact request data an upstream may receive. It is intentionally not
/// `Debug`, preventing accidental header/key logging.
#[derive(Clone)]
pub struct RequestProjection {
    pub provider: ProviderId,
    pub model_id: String,
    pub endpoint: Url,
    authorization: String,
}

impl RequestProjection {
    pub fn authorization(&self) -> &str {
        &self.authorization
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: String,
    pub wire_id: String,
    pub provider: ProviderId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CatalogCacheKey {
    provider: ProviderId,
    policy: String,
    key_fingerprint: String,
    origin: String,
}

/// Fixed adapter registry. No user-configured endpoint is accepted for these
/// adapters: this prevents a credential saved for one provider reaching a
/// look-alike or cross-origin endpoint.
#[derive(Default)]
pub struct ProviderAdapterRegistry {
    cache: HashMap<CatalogCacheKey, Vec<CatalogModel>>,
}

impl ProviderAdapterRegistry {
    pub fn request_projection(
        &self,
        provider: ProviderId,
        model_id: &str,
        key: ApiKey,
    ) -> Result<RequestProjection, ProviderAuthRequiredError> {
        if model_id.trim().is_empty() {
            return Err(provider_auth_required(provider));
        }
        Ok(RequestProjection {
            provider,
            // The catalog key is canonical (`provider/model`), but upstreams
            // only accept their provider-owned wire model identifier.
            model_id: model_id.to_owned(),
            endpoint: Url::parse(endpoint_base(provider)).expect("fixed provider URL"),
            authorization: format!("Bearer {}", key.expose()),
        })
    }

    pub fn projection_from_store(
        &self,
        store: &ProviderCredentialStore,
        provider: ProviderId,
        model_id: &str,
    ) -> Result<RequestProjection, ProviderAuthRequiredError> {
        match store.api_key(provider) {
            Ok(Some(key)) => self.request_projection(provider, model_id, key),
            Ok(None) | Err(_) => Err(provider_auth_required(provider)),
        }
    }

    /// Discover one provider through an injected transport. Failure preserves
    /// that provider's prior cache and cannot modify another provider's cache.
    pub fn discover_with<F>(
        &mut self,
        provider: ProviderId,
        key: ApiKey,
        fetch: F,
    ) -> Result<Vec<CatalogModel>>
    where
        F: FnOnce(&RequestProjection) -> Result<Value>,
    {
        let mut request = self
            .request_projection(provider, "__catalog__", key.clone())
            .map_err(|_| anyhow!("provider credential unavailable"))?;
        request.endpoint =
            Url::parse(catalog_endpoint(provider)).expect("fixed provider catalog URL");
        let cache_key = CatalogCacheKey {
            provider,
            policy: "unfiltered-v1".to_owned(),
            key_fingerprint: key.fingerprint(),
            origin: request.endpoint.origin().ascii_serialization(),
        };
        let response = fetch(&request);
        match response.and_then(|value| parse_catalog(provider, value)) {
            Ok(models) => {
                self.cache.insert(cache_key, models.clone());
                Ok(models)
            }
            Err(error) => self.cache.get(&cache_key).cloned().ok_or(error),
        }
    }

    /// Discover a provider catalog and retain only models explicitly
    /// registered by configuration. This is the compatibility boundary:
    /// OpenAI's basic `data[].id` response never implies agent/backend
    /// compatibility, and an unregistered remote model cannot enter `/model`.
    pub fn discover_registered_with<F>(
        &mut self,
        provider: ProviderId,
        key: ApiKey,
        registered: &[CatalogModel],
        fetch: F,
    ) -> Result<Vec<CatalogModel>>
    where
        F: FnOnce(&RequestProjection) -> Result<Value>,
    {
        let mut request = self
            .request_projection(provider, "__catalog__", key.clone())
            .map_err(|_| anyhow!("provider credential unavailable"))?;
        request.endpoint =
            Url::parse(catalog_endpoint(provider)).expect("fixed provider catalog URL");
        let cache_key = registered_cache_key(provider, &key, &request.endpoint, registered);
        let response = fetch(&request).and_then(|value| parse_catalog(provider, value));
        match response {
            Ok(discovered) => {
                let discovered_wire_ids: std::collections::HashSet<&str> = discovered
                    .iter()
                    .map(|model| model.wire_id.as_str())
                    .collect();
                let models: Vec<_> = registered
                    .iter()
                    .filter(|model| {
                        model.provider == provider
                            && discovered_wire_ids.contains(model.wire_id.as_str())
                    })
                    .cloned()
                    .collect();
                self.cache.insert(cache_key, models.clone());
                Ok(models)
            }
            Err(error) => self.cache.get(&cache_key).cloned().ok_or(error),
        }
    }

    /// Production HTTP transport for fixed Provider model discovery.
    pub async fn discover_registered_http(
        &mut self,
        store: &ProviderCredentialStore,
        provider: ProviderId,
        registered: &[CatalogModel],
        client: &reqwest::Client,
    ) -> Result<Vec<CatalogModel>> {
        let key = store
            .api_key(provider)?
            .ok_or_else(|| anyhow!("provider credential unavailable"))?;
        let mut request = self
            .request_projection(provider, "__catalog__", key.clone())
            .map_err(|_| anyhow!("provider credential unavailable"))?;
        request.endpoint =
            Url::parse(catalog_endpoint(provider)).expect("fixed provider catalog URL");
        let cache_key = registered_cache_key(provider, &key, &request.endpoint, registered);
        let response = async {
            let response = client
                .get(request.endpoint.clone())
                .bearer_auth(key.expose())
                .send()
                .await
                .context("requesting provider model catalog")?
                .error_for_status()
                .context("provider model catalog returned an error status")?;
            let value = response
                .json::<Value>()
                .await
                .context("decoding provider model catalog")?;
            parse_catalog(provider, value)
        }
        .await;
        match response {
            Ok(discovered) => {
                let discovered_wire_ids: std::collections::HashSet<&str> = discovered
                    .iter()
                    .map(|model| model.wire_id.as_str())
                    .collect();
                let models: Vec<_> = registered
                    .iter()
                    .filter(|model| {
                        model.provider == provider
                            && discovered_wire_ids.contains(model.wire_id.as_str())
                    })
                    .cloned()
                    .collect();
                self.cache.insert(cache_key, models.clone());
                Ok(models)
            }
            Err(error) => self.cache.get(&cache_key).cloned().ok_or(error),
        }
    }

    pub fn cached_models(&self, provider: ProviderId, key: &ApiKey) -> Option<&[CatalogModel]> {
        let cache_key = CatalogCacheKey {
            provider,
            policy: "unfiltered-v1".to_owned(),
            key_fingerprint: key.fingerprint(),
            origin: Url::parse(endpoint_base(provider))
                .expect("fixed provider URL")
                .origin()
                .ascii_serialization(),
        };
        self.cache.get(&cache_key).map(Vec::as_slice)
    }
}

fn registered_cache_key(
    provider: ProviderId,
    key: &ApiKey,
    endpoint: &Url,
    registered: &[CatalogModel],
) -> CatalogCacheKey {
    let mut policy_rows: Vec<_> = registered
        .iter()
        .filter(|model| model.provider == provider)
        .map(|model| format!("{}\0{}", model.id, model.wire_id))
        .collect();
    policy_rows.sort();
    let policy = format!(
        "registered-v1:{}",
        blake3::hash(policy_rows.join("\n").as_bytes()).to_hex()
    );
    CatalogCacheKey {
        provider,
        policy,
        key_fingerprint: key.fingerprint(),
        origin: endpoint.origin().ascii_serialization(),
    }
}

pub(crate) fn endpoint_base(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Openai => "https://api.openai.com/v1",
        ProviderId::Openrouter => "https://openrouter.ai/api/v1",
    }
}

fn catalog_endpoint(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Openai => OPENAI_MODELS_URL,
        ProviderId::Openrouter => OPENROUTER_MODELS_URL,
    }
}

fn provider_auth_required(provider: ProviderId) -> ProviderAuthRequiredError {
    ProviderAuthRequiredError {
        code: crate::agent::auth_method::PROVIDER_AUTH_REQUIRED_CODE.to_owned(),
        provider: provider.as_str().to_owned(),
        guidance: format!(
            "Run `/provider {}` to configure authentication, then retry.",
            provider.as_str()
        ),
    }
}

fn parse_catalog(provider: ProviderId, value: Value) -> Result<Vec<CatalogModel>> {
    let raw_models = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("provider catalog has no data array"))?;
    let mut models = BTreeMap::new();
    for model in raw_models {
        let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        if provider == ProviderId::Openrouter && !openrouter_model_is_usable(model) {
            continue;
        }
        models.insert(
            id.to_owned(),
            CatalogModel {
                id: format!("{}/{}", provider.as_str(), id),
                wire_id: id.to_owned(),
                provider,
            },
        );
    }
    Ok(models.into_values().collect())
}

fn openrouter_model_is_usable(model: &Value) -> bool {
    let architecture = model.get("architecture").unwrap_or(&Value::Null);
    let modality = architecture
        .get("modality")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input_modalities = architecture
        .get("input_modalities")
        .and_then(Value::as_array);
    let text_input = modality.contains("text")
        || input_modalities
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("text")));
    let tool_capable = model
        .pointer("/supported_parameters")
        .and_then(Value::as_array)
        .is_some_and(|parameters| {
            parameters
                .iter()
                .any(|item| matches!(item.as_str(), Some("tools") | Some("tool_choice")))
        });
    text_input && tool_capable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_legacy_xai_api_key(directory: &std::path::Path, key: &str) {
        let mut legacy = std::collections::BTreeMap::new();
        legacy.insert(
            super::super::model::API_KEY_SCOPE.to_owned(),
            super::super::model::GrokAuth {
                key: key.to_owned(),
                auth_mode: super::super::model::AuthMode::ApiKey,
                ..Default::default()
            },
        );
        fs::write(
            directory.join("auth.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
    }

    fn key(value: &str) -> ApiKey {
        ApiKey::new(value).unwrap()
    }

    #[test]
    fn tc9_provider_key_is_namespaced_and_masked() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        let secret = "sk-openai-top-secret";
        store
            .store_api_key(ProviderId::Openai, key(secret))
            .unwrap();
        let raw = fs::read_to_string(directory.path().join("auth.json")).unwrap();
        let json: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            json["providers"]["openai"]["credentials"]["api_key"]["type"],
            "api_key"
        );
        assert!(
            json.pointer("/providers/openai/credentials/api_key/endpoint")
                .is_none()
        );
        assert!(
            !format!("{:?}", store.api_key(ProviderId::Openai).unwrap().unwrap()).contains(secret)
        );
    }

    #[test]
    fn auth_manager_writes_preserve_other_provider_namespaces() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        store
            .store_api_key(ProviderId::Openai, key("openai-secret"))
            .unwrap();
        // This is the established xAI AuthManager storage facade. Its v2
        // rewrite must retain the OpenAI namespace introduced above.
        super::super::storage::store_api_key(directory.path(), "xai-secret").unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(directory.path().join("auth.json")).unwrap()).unwrap();
        assert_eq!(
            raw["providers"]["openai"]["credentials"]["api_key"]["type"],
            "api_key"
        );
    }

    #[test]
    fn provider_write_preserves_existing_xai_credential() {
        let directory = tempfile::tempdir().unwrap();
        super::super::storage::store_api_key(directory.path(), "xai-secret").unwrap();

        ProviderCredentialStore::new(directory.path())
            .store_api_key(ProviderId::Openai, key("openai-secret"))
            .unwrap();

        assert_eq!(
            super::super::storage::read_api_key(directory.path()).as_deref(),
            Some("xai-secret")
        );
    }

    #[test]
    fn provider_writer_migrates_locked_legacy_xai_without_losing_it() {
        let directory = tempfile::tempdir().unwrap();
        write_legacy_xai_api_key(directory.path(), "legacy-xai-secret");

        ProviderCredentialStore::new(directory.path())
            .store_api_key(ProviderId::Openrouter, key("router-secret"))
            .unwrap();

        assert_eq!(
            super::super::storage::read_api_key(directory.path()).as_deref(),
            Some("legacy-xai-secret"),
            "the provider writer must migrate the bytes it re-read after acquiring auth.json.lock"
        );
        let raw: Value =
            serde_json::from_slice(&fs::read(directory.path().join("auth.json")).unwrap()).unwrap();
        assert_eq!(
            raw["providers"]["openrouter"]["credentials"]["api_key"]["key"],
            "router-secret"
        );
    }

    #[test]
    fn clear_xai_key_keeps_provider_only_auth_file() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        store
            .store_api_key(ProviderId::Openrouter, key("router-secret"))
            .unwrap();
        super::super::storage::clear_api_key(directory.path()).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(directory.path().join("auth.json")).unwrap()).unwrap();
        assert_eq!(
            raw["providers"]["openrouter"]["credentials"]["api_key"]["type"],
            "api_key"
        );
    }

    #[test]
    fn clearing_xai_key_preserves_mixed_provider_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        super::super::storage::store_api_key(directory.path(), "xai-secret").unwrap();
        store
            .store_api_key(ProviderId::Openrouter, key("router-secret"))
            .unwrap();

        super::super::storage::clear_api_key(directory.path()).unwrap();

        assert_eq!(super::super::storage::read_api_key(directory.path()), None);
        assert_eq!(
            store
                .api_key(ProviderId::Openrouter)
                .unwrap()
                .unwrap()
                .expose(),
            "router-secret"
        );
    }

    #[cfg(unix)]
    #[test]
    fn provider_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        store
            .store_api_key(ProviderId::Openai, key("openai-secret"))
            .unwrap();
        assert_eq!(
            fs::metadata(directory.path().join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn provider_store_creates_owner_only_auth_directory() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let grok_home = directory.path().join("new-grok-home");
        ProviderCredentialStore::new(&grok_home)
            .store_api_key(ProviderId::Openai, key("openai-secret"))
            .unwrap();

        assert_eq!(
            fs::metadata(&grok_home).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn api_key_update_preserves_other_credential_strategies() {
        let directory = tempfile::tempdir().unwrap();
        let auth_path = directory.path().join("auth.json");
        fs::write(
            &auth_path,
            serde_json::to_vec(&json!({
                "version": 2,
                "providers": {
                    "openai": {
                        "credentials": {
                            "oauth": {
                                "type": "oauth",
                                "refresh_token": "opaque-refresh-token"
                            }
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        ProviderCredentialStore::with_path(&auth_path)
            .store_api_key(ProviderId::Openai, key("openai-secret"))
            .unwrap();

        let raw: Value = serde_json::from_slice(&fs::read(auth_path).unwrap()).unwrap();
        assert_eq!(
            raw["providers"]["openai"]["credentials"]["oauth"]["refresh_token"],
            "opaque-refresh-token"
        );
        assert_eq!(
            raw["providers"]["openai"]["credentials"]["api_key"]["key"],
            "openai-secret"
        );
    }

    #[test]
    fn provider_writer_refuses_future_store_version() {
        let directory = tempfile::tempdir().unwrap();
        let auth_path = directory.path().join("auth.json");
        let original = serde_json::to_vec(&json!({
            "version": 3,
            "providers": {
                "openai": {
                    "credentials": {
                        "api_key": { "type": "api_key", "key": "future-secret" }
                    }
                }
            }
        }))
        .unwrap();
        fs::write(&auth_path, &original).unwrap();

        assert!(
            ProviderCredentialStore::with_path(&auth_path)
                .store_api_key(ProviderId::Openai, key("replacement"))
                .is_err()
        );
        assert_eq!(fs::read(auth_path).unwrap(), original);
    }

    #[test]
    fn tc11_projection_never_mixes_provider_origin_or_key() {
        let registry = ProviderAdapterRegistry::default();
        let openai = registry
            .request_projection(ProviderId::Openai, "gpt-4.1", key("openai-secret"))
            .unwrap();
        let router = registry
            .request_projection(ProviderId::Openrouter, "vendor/model", key("router-secret"))
            .unwrap();
        assert_eq!(openai.endpoint.as_str(), "https://api.openai.com/v1");
        assert_eq!(router.endpoint.as_str(), "https://openrouter.ai/api/v1");
        assert_eq!(openai.model_id, "gpt-4.1");
        assert_eq!(router.model_id, "vendor/model");
        assert!(openai.authorization().contains("openai-secret"));
        assert!(!openai.authorization().contains("router-secret"));
        assert!(router.authorization().contains("router-secret"));
        assert!(!router.authorization().contains("openai-secret"));
    }

    #[test]
    fn tc11_sampling_credentials_use_store_and_fixed_origin() {
        let directory = tempfile::tempdir().unwrap();
        ProviderCredentialStore::new(directory.path())
            .store_api_key(ProviderId::Openai, key("stored-openai-secret"))
            .unwrap();
        let mut info = crate::agent::config::ModelInfo::fallback("gpt-4.1");
        info.id = Some("openai/gpt-4.1".to_owned());
        info.base_url = "https://attacker.invalid/v1".to_owned();
        let model = crate::agent::config::ModelEntry {
            info,
            api_key: Some("config-secret-that-must-not-cross-origin".to_owned()),
            env_key: None,
            api_base_url: Some("https://attacker.invalid/v1".to_owned()),
        };

        let credentials =
            crate::agent::config::resolve_fixed_provider_credentials_at(&model, directory.path())
                .unwrap();
        assert_eq!(credentials.api_key.as_deref(), Some("stored-openai-secret"));
        assert_eq!(credentials.base_url, "https://api.openai.com/v1");
        assert_eq!(credentials.auth_type, xai_chat_state::AuthType::ApiKey);
    }

    #[test]
    fn tc19_catalog_protocols_and_cache_are_provider_isolated() {
        let mut registry = ProviderAdapterRegistry::default();
        let openai_key = key("openai-secret");
        let router_key = key("router-secret");
        let openai = registry
            .discover_with(ProviderId::Openai, openai_key.clone(), |request| {
                assert_eq!(request.endpoint.as_str(), OPENAI_MODELS_URL);
                Ok(json!({"data":[{"id":"gpt-4.1"}]}))
            })
            .unwrap();
        let router = registry
            .discover_with(ProviderId::Openrouter, router_key.clone(), |request| {
                assert_eq!(request.endpoint.as_str(), OPENROUTER_MODELS_URL);
                Ok(json!({"data":[
                    {
                        "id":"vendor/model",
                        "architecture":{"modality":"text->text"},
                        "supported_parameters":["tools"]
                    },
                    {
                        "id":"text-without-tools",
                        "architecture":{"modality":"text->text"},
                        "supported_parameters":[]
                    },
                    {
                        "id":"image-with-tools",
                        "architecture":{"modality":"image->image"},
                        "supported_parameters":["tools"]
                    }
                ]}))
            })
            .unwrap();
        assert_eq!(openai[0].id, "openai/gpt-4.1");
        assert_eq!(router[0].id, "openrouter/vendor/model");
        assert_eq!(
            registry
                .cached_models(ProviderId::Openai, &openai_key)
                .unwrap(),
            openai.as_slice()
        );
        assert_eq!(
            registry
                .cached_models(ProviderId::Openrouter, &router_key)
                .unwrap(),
            router.as_slice()
        );
    }

    #[test]
    fn openai_discovery_only_admits_explicitly_registered_models() {
        let mut registry = ProviderAdapterRegistry::default();
        let registered = vec![
            CatalogModel {
                id: "openai/gpt-approved".to_owned(),
                wire_id: "gpt-approved".to_owned(),
                provider: ProviderId::Openai,
            },
            CatalogModel {
                id: "openai/gpt-not-returned".to_owned(),
                wire_id: "gpt-not-returned".to_owned(),
                provider: ProviderId::Openai,
            },
        ];

        let models = registry
            .discover_registered_with(
                ProviderId::Openai,
                key("openai-secret"),
                &registered,
                |_| {
                    Ok(json!({
                        "data": [
                            { "id": "gpt-approved" },
                            { "id": "remote-but-unregistered" }
                        ]
                    }))
                },
            )
            .unwrap();

        assert_eq!(
            models,
            vec![CatalogModel {
                id: "openai/gpt-approved".to_owned(),
                wire_id: "gpt-approved".to_owned(),
                provider: ProviderId::Openai,
            }]
        );
    }

    #[test]
    fn registered_catalog_cache_is_bound_to_registration_policy() {
        let mut registry = ProviderAdapterRegistry::default();
        let key = key("openai-secret");
        let first = vec![CatalogModel {
            id: "openai/gpt-first".to_owned(),
            wire_id: "gpt-first".to_owned(),
            provider: ProviderId::Openai,
        }];
        registry
            .discover_registered_with(ProviderId::Openai, key.clone(), &first, |_| {
                Ok(json!({ "data": [{ "id": "gpt-first" }] }))
            })
            .unwrap();

        let changed = vec![CatalogModel {
            id: "openai/gpt-second".to_owned(),
            wire_id: "gpt-second".to_owned(),
            provider: ProviderId::Openai,
        }];
        let error = registry
            .discover_registered_with(ProviderId::Openai, key, &changed, |_| {
                Err(anyhow!("offline"))
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "offline");
    }

    #[test]
    fn tc20_discovery_failure_keeps_own_cache_and_other_provider() {
        let mut registry = ProviderAdapterRegistry::default();
        let openai_key = key("openai-secret");
        let router_key = key("router-secret");
        registry
            .discover_with(ProviderId::Openai, openai_key.clone(), |_| {
                Ok(json!({"data":[{"id":"gpt"}]}))
            })
            .unwrap();
        registry
            .discover_with(ProviderId::Openrouter, router_key.clone(), |_| {
                Ok(json!({"data":[{
                    "id":"vendor/model",
                    "architecture":{"modality":"text"},
                    "supported_parameters":["tool_choice"]
                }]}))
            })
            .unwrap();
        let preserved = registry
            .discover_with(ProviderId::Openai, openai_key.clone(), |_| {
                Err(anyhow!("offline"))
            })
            .unwrap();
        assert_eq!(preserved[0].id, "openai/gpt");
        assert_eq!(
            registry
                .cached_models(ProviderId::Openrouter, &router_key)
                .unwrap()[0]
                .id,
            "openrouter/vendor/model"
        );
    }
}
