//! Provider-owned API-key routing and model discovery.
//!
//! This module deliberately keeps a provider credential separate from its
//! endpoint.  In particular, callers must obtain a [`RequestProjection`] from
//! the registry instead of combining fields from arbitrary model entries.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::agent::auth_method::ProviderAuthRequiredError;

const AUTH_STORE_VERSION: u8 = 2;
const OPENAI_MODELS_URL: &str = "https://api.openai.com/v1/models";
const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
const ANTHROPIC_MODELS_URL: &str = "https://api.anthropic.com/v1/models";
const GEMINI_MODELS_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";

/// Providers with a fixed, reviewed protocol in the initial registry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Anthropic,
    Gemini,
    Openai,
    Openrouter,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStability {
    Stable,
    Conditional,
    Experimental,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterAvailability {
    Available,
    NotEnabled,
    BlockedExternal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthMethod {
    pub id: &'static str,
    pub provider: &'static str,
    pub stability: AdapterStability,
    pub availability: AdapterAvailability,
    pub requires_confirmation: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProviderReleaseGates {
    pub gemini_oauth: bool,
    pub github_copilot: bool,
    pub codex_oauth: bool,
    pub claude_oauth: bool,
}

impl ProviderReleaseGates {
    pub fn from_features(features: &crate::agent::config::Features) -> Self {
        Self {
            gemini_oauth: features.provider_gemini_oauth.unwrap_or(false),
            github_copilot: features.provider_github_copilot.unwrap_or(false),
            codex_oauth: features.provider_codex_oauth.unwrap_or(false),
            claude_oauth: features.provider_claude_oauth.unwrap_or(false),
        }
    }
}

pub fn provider_auth_methods(
    gates: ProviderReleaseGates,
    copilot_external_ready: bool,
    gemini_oauth_ready: bool,
) -> Vec<ProviderAuthMethod> {
    let mut methods = vec![
        ProviderAuthMethod {
            id: "openai_api_key",
            provider: "openai",
            stability: AdapterStability::Stable,
            availability: AdapterAvailability::Available,
            requires_confirmation: false,
        },
        ProviderAuthMethod {
            id: "anthropic_api_key",
            provider: "anthropic",
            stability: AdapterStability::Stable,
            availability: AdapterAvailability::Available,
            requires_confirmation: false,
        },
        ProviderAuthMethod {
            id: "gemini_api_key",
            provider: "gemini",
            stability: AdapterStability::Stable,
            availability: AdapterAvailability::Available,
            requires_confirmation: false,
        },
        ProviderAuthMethod {
            id: "openrouter_api_key",
            provider: "openrouter",
            stability: AdapterStability::Stable,
            availability: AdapterAvailability::Available,
            requires_confirmation: false,
        },
    ];
    if gates.gemini_oauth {
        methods.push(ProviderAuthMethod {
            id: "gemini_oauth",
            provider: "gemini",
            stability: AdapterStability::Conditional,
            availability: if gemini_oauth_ready {
                AdapterAvailability::Available
            } else {
                AdapterAvailability::NotEnabled
            },
            requires_confirmation: false,
        });
    }
    if gates.github_copilot {
        methods.push(ProviderAuthMethod {
            id: "github_copilot",
            provider: "github-copilot",
            stability: AdapterStability::Conditional,
            availability: if copilot_external_ready {
                AdapterAvailability::NotEnabled
            } else {
                AdapterAvailability::BlockedExternal
            },
            requires_confirmation: false,
        });
    }
    if gates.codex_oauth {
        methods.push(ProviderAuthMethod {
            id: "codex_oauth_compat",
            provider: "openai",
            stability: AdapterStability::Experimental,
            availability: AdapterAvailability::NotEnabled,
            requires_confirmation: true,
        });
    }
    if gates.claude_oauth {
        methods.push(ProviderAuthMethod {
            id: "claude_oauth_compat",
            provider: "anthropic",
            stability: AdapterStability::Experimental,
            availability: AdapterAvailability::NotEnabled,
            requires_confirmation: true,
        });
    }
    methods
}

impl ProviderId {
    pub const ALL: [Self; 4] = [
        Self::Anthropic,
        Self::Gemini,
        Self::Openai,
        Self::Openrouter,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
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
            "anthropic" => Some(Self::Anthropic),
            "gemini" => Some(Self::Gemini),
            "openai" => Some(Self::Openai),
            "openrouter" => Some(Self::Openrouter),
            _ => None,
        }
    }
}

fn validate_custom_provider_id(provider: &str) -> Result<()> {
    if provider.is_empty()
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || matches!(
            provider,
            "xai" | "anthropic" | "gemini" | "openai" | "openrouter" | "github-copilot"
        )
    {
        bail!("invalid custom Provider id");
    }
    Ok(())
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOAuthCredential {
    pub strategy: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl std::fmt::Debug for ProviderOAuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderOAuthCredential")
            .field("strategy", &self.strategy)
            .field("has_access_token", &!self.access_token.is_empty())
            .field("has_refresh_token", &self.refresh_token.is_some())
            .field("expires_at", &self.expires_at)
            .field("token_endpoint", &self.token_endpoint)
            .field("client_id", &self.client_id)
            .field("source", &self.source)
            .finish()
    }
}

impl ProviderOAuthCredential {
    pub(crate) fn refresh_fingerprint(&self) -> Option<String> {
        self.refresh_token
            .as_ref()
            .map(|token| blake3::hash(token.as_bytes()).to_hex().to_string())
    }

    pub fn is_expired_or_near_expiry(&self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| expires_at <= chrono::Utc::now().timestamp() + 60)
    }

    fn validate(&self) -> Result<()> {
        if self.strategy.trim().is_empty()
            || self.access_token.trim().is_empty()
            || self.token_endpoint.trim().is_empty()
            || self.client_id.trim().is_empty()
        {
            bail!("provider OAuth credential is incomplete");
        }
        let endpoint = Url::parse(&self.token_endpoint).context("invalid OAuth token endpoint")?;
        if endpoint.scheme() != "https" && !cfg!(test) {
            bail!("provider OAuth token endpoint must use HTTPS");
        }
        Ok(())
    }
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
        self.store_api_key_in_namespace(provider.as_str(), "api_key", key)
    }

    /// Store a named custom Provider credential in its own namespace.
    ///
    /// The credential id intentionally equals the Provider id, matching the
    /// v2 store contract and preventing a singleton `custom` credential from
    /// being shared by unrelated upstreams.
    pub fn store_custom_api_key(&self, provider: &str, key: ApiKey) -> Result<()> {
        validate_custom_provider_id(provider)?;
        self.store_api_key_in_namespace(provider, provider, key)
    }

    fn store_api_key_in_namespace(
        &self,
        provider: &str,
        credential_id: &str,
        key: ApiKey,
    ) -> Result<()> {
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
            .entry(provider.to_owned())
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
            credential_id.to_owned(),
            json!({ "type": "api_key", "key": key.expose() }),
        );
        if !lock.still_live(&self.path) {
            bail!("auth store lock was replaced before provider credential write");
        }
        self.write_raw(&root)
    }

    pub fn api_key(&self, provider: ProviderId) -> Result<Option<ApiKey>> {
        self.api_key_in_namespace(provider.as_str(), "api_key")
    }

    pub fn custom_api_key(&self, provider: &str) -> Result<Option<ApiKey>> {
        validate_custom_provider_id(provider)?;
        self.api_key_in_namespace(provider, provider)
    }

    fn api_key_in_namespace(&self, provider: &str, credential_id: &str) -> Result<Option<ApiKey>> {
        let root = self.read_raw()?;
        let value = root.pointer(&format!(
            "/providers/{provider}/credentials/{credential_id}"
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

    pub fn store_oauth(
        &self,
        provider: ProviderId,
        credential: ProviderOAuthCredential,
    ) -> Result<()> {
        credential.validate()?;
        let lock = self.lock_with_timeout(Duration::from_secs(5))?;
        let mut root = self.read_provider_root_while_locked(&lock)?;
        self.write_oauth_while_locked(provider, &credential, &mut root, &lock)
    }

    pub fn oauth(
        &self,
        provider: ProviderId,
        strategy: &str,
    ) -> Result<Option<ProviderOAuthCredential>> {
        let root = self.read_raw()?;
        let value = root.pointer(&format!(
            "/providers/{}/credentials/oauth",
            provider.as_str()
        ));
        let Some(value) = value else {
            return Ok(None);
        };
        let credential: ProviderOAuthCredential =
            serde_json::from_value(value.clone()).context("invalid provider OAuth credential")?;
        if credential.strategy != strategy {
            return Ok(None);
        }
        credential.validate()?;
        Ok(Some(credential))
    }

    pub fn clear_oauth(&self, provider: ProviderId, strategy: &str) -> Result<bool> {
        let lock = self.lock_with_timeout(Duration::from_secs(5))?;
        let mut root = self.read_provider_root_while_locked(&lock)?;
        self.remove_oauth_while_locked(provider, strategy, &mut root, &lock)
    }

    /// Provider+credential cross-process refresh singleflight.
    ///
    /// The caller supplies the refresh-token fingerprint observed before
    /// waiting. After taking the shared `auth.json` lock this method re-reads
    /// disk. A rotated token is reused immediately; otherwise `refresh` is
    /// invoked exactly once while the lock is held and its result is installed
    /// atomically before another process can consume the old refresh token.
    pub fn refresh_oauth_with<F>(
        &self,
        provider: ProviderId,
        strategy: &str,
        observed_refresh_fingerprint: Option<&str>,
        refresh: F,
    ) -> Result<ProviderOAuthCredential>
    where
        F: FnOnce(&ProviderOAuthCredential) -> Result<ProviderOAuthCredential>,
    {
        let lock = self.lock_with_timeout(Duration::from_secs(10))?;
        let mut root = self.read_provider_root_while_locked(&lock)?;
        let current_value = root
            .pointer(&format!(
                "/providers/{}/credentials/oauth",
                provider.as_str()
            ))
            .cloned()
            .ok_or_else(|| anyhow!("provider OAuth credential unavailable"))?;
        let current: ProviderOAuthCredential =
            serde_json::from_value(current_value).context("invalid provider OAuth credential")?;
        current.validate()?;
        if current.strategy != strategy {
            bail!("provider OAuth credential strategy mismatch");
        }
        let current_fingerprint = current.refresh_fingerprint();
        if observed_refresh_fingerprint.is_some()
            && current_fingerprint.as_deref() != observed_refresh_fingerprint
        {
            return Ok(current);
        }
        if current.refresh_token.is_none() {
            bail!("provider OAuth credential has no refresh token");
        }
        let refreshed = refresh(&current)?;
        refreshed.validate()?;
        if refreshed.strategy != strategy {
            bail!("refreshed OAuth credential strategy mismatch");
        }
        self.write_oauth_while_locked(provider, &refreshed, &mut root, &lock)?;
        Ok(refreshed)
    }

    pub async fn refresh_oauth_http(
        &self,
        provider: ProviderId,
        strategy: &str,
        observed_refresh_fingerprint: Option<&str>,
        client: &reqwest::Client,
    ) -> Result<ProviderOAuthCredential> {
        self.refresh_oauth_http_inner(
            provider,
            strategy,
            observed_refresh_fingerprint,
            client,
            None,
        )
        .await
    }

    pub async fn refresh_oauth_http_with_client_secret(
        &self,
        provider: ProviderId,
        strategy: &str,
        observed_refresh_fingerprint: Option<&str>,
        client: &reqwest::Client,
        client_secret: &str,
    ) -> Result<ProviderOAuthCredential> {
        if client_secret.is_empty() {
            bail!("provider OAuth client secret is empty");
        }
        self.refresh_oauth_http_inner(
            provider,
            strategy,
            observed_refresh_fingerprint,
            client,
            Some(client_secret),
        )
        .await
    }

    async fn refresh_oauth_http_inner(
        &self,
        provider: ProviderId,
        strategy: &str,
        observed_refresh_fingerprint: Option<&str>,
        client: &reqwest::Client,
        client_secret: Option<&str>,
    ) -> Result<ProviderOAuthCredential> {
        let lock = self.lock_with_timeout(Duration::from_secs(10))?;
        let mut root = self.read_provider_root_while_locked(&lock)?;
        let current_value = root
            .pointer(&format!(
                "/providers/{}/credentials/oauth",
                provider.as_str()
            ))
            .cloned()
            .ok_or_else(|| anyhow!("provider OAuth credential unavailable"))?;
        let current: ProviderOAuthCredential =
            serde_json::from_value(current_value).context("invalid provider OAuth credential")?;
        current.validate()?;
        if current.strategy != strategy {
            bail!("provider OAuth credential strategy mismatch");
        }
        let current_fingerprint = current.refresh_fingerprint();
        if observed_refresh_fingerprint.is_some()
            && current_fingerprint.as_deref() != observed_refresh_fingerprint
        {
            return Ok(current);
        }
        let refresh_token = current
            .refresh_token
            .as_deref()
            .ok_or_else(|| anyhow!("provider OAuth credential has no refresh token"))?;
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", current.client_id.as_str()),
        ];
        if let Some(client_secret) = client_secret {
            form.push(("client_secret", client_secret));
        }
        let response = client
            .post(&current.token_endpoint)
            .form(&form)
            .send()
            .await
            .context("requesting provider OAuth token refresh")?;
        let status = response.status();
        let value = response
            .json::<Value>()
            .await
            .context("decoding provider OAuth token refresh")?;
        if !status.is_success() {
            let terminal = matches!(status.as_u16(), 400 | 401 | 403)
                && value
                    .get("error")
                    .and_then(Value::as_str)
                    .is_some_and(|error| {
                        matches!(
                            error,
                            "invalid_grant" | "invalid_client" | "unauthorized_client"
                        )
                    });
            if terminal {
                self.remove_oauth_while_locked(provider, strategy, &mut root, &lock)?;
                bail!("provider OAuth credential was rejected and removed");
            }
            bail!("provider OAuth refresh returned status {}", status.as_u16());
        }
        let access_token = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| anyhow!("provider OAuth refresh returned no access token"))?;
        let expires_at = value
            .get("expires_in")
            .and_then(Value::as_i64)
            .map(|seconds| chrono::Utc::now().timestamp().saturating_add(seconds));
        let refreshed = ProviderOAuthCredential {
            strategy: current.strategy,
            access_token: access_token.to_owned(),
            refresh_token: value
                .get("refresh_token")
                .and_then(Value::as_str)
                .filter(|token| !token.is_empty())
                .map(str::to_owned)
                .or(current.refresh_token),
            expires_at,
            token_endpoint: current.token_endpoint,
            client_id: current.client_id,
            source: current.source,
        };
        refreshed.validate()?;
        self.write_oauth_while_locked(provider, &refreshed, &mut root, &lock)?;
        Ok(refreshed)
    }

    fn lock_with_timeout(&self, timeout: Duration) -> Result<super::storage::AuthFileLock> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("auth.json has no parent"))?;
        if !parent.exists() {
            fs::create_dir_all(parent).context("creating auth.json directory")?;
            super::storage::set_secure_directory_permissions(parent)
                .context("securing auth.json directory")?;
        }
        let started = Instant::now();
        loop {
            if let Some(lock) = super::manager::lock::try_lock_auth_file_nonblocking(&self.path) {
                return Ok(lock);
            }
            if started.elapsed() >= timeout {
                bail!("timed out waiting for provider credential refresh lock");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn write_oauth_while_locked(
        &self,
        provider: ProviderId,
        credential: &ProviderOAuthCredential,
        root: &mut Value,
        lock: &super::storage::AuthFileLock,
    ) -> Result<()> {
        if !root.is_object() {
            *root = json!({});
        }
        root["version"] = json!(AUTH_STORE_VERSION);
        let providers = root
            .as_object_mut()
            .expect("object checked above")
            .entry("providers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| anyhow!("auth store providers is not an object"))?;
        let credentials = providers
            .entry(provider.as_str().to_owned())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| anyhow!("provider auth entry is not an object"))?
            .entry("credentials")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| anyhow!("provider credentials entry is not an object"))?;
        credentials.insert(
            "oauth".to_owned(),
            serde_json::to_value(credential).context("serializing provider OAuth credential")?,
        );
        if !lock.still_live(&self.path) {
            bail!("auth store lock was replaced before OAuth credential write");
        }
        self.write_raw(root)
    }

    fn read_provider_root_while_locked(
        &self,
        lock: &super::storage::AuthFileLock,
    ) -> Result<Value> {
        let mut root = self.read_raw()?;
        if !root.get("providers").is_some_and(Value::is_object) && self.path.exists() {
            super::storage::migrate_legacy_store_while_locked(&self.path, lock)
                .context("migrating locked legacy auth.json")?;
            root = self.read_raw()?;
        }
        Ok(root)
    }

    fn remove_oauth_while_locked(
        &self,
        provider: ProviderId,
        strategy: &str,
        root: &mut Value,
        lock: &super::storage::AuthFileLock,
    ) -> Result<bool> {
        let Some(credentials) = root
            .pointer_mut(&format!("/providers/{}/credentials", provider.as_str()))
            .and_then(Value::as_object_mut)
        else {
            return Ok(false);
        };
        let matches = credentials
            .get("oauth")
            .and_then(|value| value.get("strategy"))
            .and_then(Value::as_str)
            == Some(strategy);
        if !matches {
            return Ok(false);
        }
        credentials.remove("oauth");
        if !lock.still_live(&self.path) {
            bail!("auth store lock was replaced before OAuth credential clear");
        }
        self.write_raw(root)?;
        Ok(true)
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
    credential_header_name: &'static str,
    credential_header_value: String,
}

impl RequestProjection {
    pub(crate) fn credential_header(&self) -> (&'static str, &str) {
        (
            self.credential_header_name,
            self.credential_header_value.as_str(),
        )
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CustomCatalogCacheKey {
    provider: String,
    key_fingerprint: String,
    origin: String,
    path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomCatalogModel {
    pub id: String,
    pub wire_id: String,
    pub provider: String,
}

#[derive(Clone)]
pub struct CustomCatalogRequest {
    pub endpoint: Url,
    credential_header_name: String,
    credential_header_value: String,
}

impl CustomCatalogRequest {
    pub(crate) fn credential_header(&self) -> (&str, &str) {
        (&self.credential_header_name, &self.credential_header_value)
    }
}

/// Fixed adapter registry. No user-configured endpoint is accepted for these
/// adapters: this prevents a credential saved for one provider reaching a
/// look-alike or cross-origin endpoint.
#[derive(Default)]
pub struct ProviderAdapterRegistry {
    cache: HashMap<CatalogCacheKey, Vec<CatalogModel>>,
    custom_cache: HashMap<CustomCatalogCacheKey, Vec<CustomCatalogModel>>,
}

impl ProviderAdapterRegistry {
    pub fn discover_custom_with<F>(
        &mut self,
        provider: &str,
        base_url: &str,
        models_path: Option<&str>,
        credential_header_name: &str,
        key: ApiKey,
        fetch: F,
    ) -> Result<Vec<CustomCatalogModel>>
    where
        F: FnOnce(&CustomCatalogRequest) -> Result<Value>,
    {
        validate_custom_provider_id(provider)?;
        let base = Url::parse(base_url).context("invalid custom Provider base URL")?;
        let path = models_path.unwrap_or("/v1/models");
        if !path.starts_with('/') || path.starts_with("//") || Url::parse(path).is_ok() {
            bail!("custom Provider models path must be an absolute URL path");
        }
        let endpoint = base
            .join(path)
            .context("invalid custom Provider models path")?;
        if endpoint.origin() != base.origin() {
            bail!("custom Provider models path changes origin");
        }
        let credential_header_value =
            if credential_header_name.eq_ignore_ascii_case("authorization") {
                format!("Bearer {}", key.expose())
            } else {
                key.expose().to_owned()
            };
        let request = CustomCatalogRequest {
            endpoint: endpoint.clone(),
            credential_header_name: credential_header_name.to_owned(),
            credential_header_value,
        };
        let cache_key = CustomCatalogCacheKey {
            provider: provider.to_owned(),
            key_fingerprint: key.fingerprint(),
            origin: endpoint.origin().ascii_serialization(),
            path: endpoint.path().to_owned(),
        };
        match fetch(&request).and_then(|value| parse_custom_catalog(provider, value)) {
            Ok(models) => {
                self.custom_cache.insert(cache_key, models.clone());
                Ok(models)
            }
            Err(error) => self.custom_cache.get(&cache_key).cloned().ok_or(error),
        }
    }

    pub async fn discover_custom_http(
        &mut self,
        provider: &str,
        base_url: &str,
        models_path: Option<&str>,
        credential_header_name: &str,
        store: &ProviderCredentialStore,
        client: &reqwest::Client,
    ) -> Result<Vec<CustomCatalogModel>> {
        let key = store
            .custom_api_key(provider)?
            .ok_or_else(|| anyhow!("custom Provider credential unavailable"))?;
        let mut captured_request = None;
        let cached_or_marker = self.discover_custom_with(
            provider,
            base_url,
            models_path,
            credential_header_name,
            key.clone(),
            |request| {
                captured_request = Some(request.clone());
                bail!("custom Provider HTTP request pending")
            },
        );
        let request = captured_request.expect("request is captured before injected fetch");
        let (header_name, header_value) = request.credential_header();
        let response = match client
            .get(request.endpoint.clone())
            .header(header_name, header_value)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return cached_or_marker,
        };
        if !response.status().is_success() {
            return cached_or_marker;
        }
        let value = match response.json::<Value>().await {
            Ok(value) => value,
            Err(_) => return cached_or_marker,
        };
        self.discover_custom_with(
            provider,
            base_url,
            models_path,
            credential_header_name,
            key,
            |_| Ok(value),
        )
    }

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
            credential_header_name: match provider {
                ProviderId::Anthropic => "x-api-key",
                ProviderId::Gemini => "x-goog-api-key",
                ProviderId::Openai | ProviderId::Openrouter => "authorization",
            },
            credential_header_value: match provider {
                ProviderId::Anthropic | ProviderId::Gemini => key.expose().to_owned(),
                ProviderId::Openai | ProviderId::Openrouter => {
                    format!("Bearer {}", key.expose())
                }
            },
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
    pub async fn discover_anthropic_http(
        &mut self,
        store: &ProviderCredentialStore,
        client: &reqwest::Client,
    ) -> Result<Vec<CatalogModel>> {
        let provider = ProviderId::Anthropic;
        let key = store
            .api_key(provider)?
            .ok_or_else(|| anyhow!("provider credential unavailable"))?;
        let endpoint = Url::parse(catalog_endpoint(provider)).expect("fixed provider catalog URL");
        let cache_key = CatalogCacheKey {
            provider,
            policy: "unfiltered-v1".to_owned(),
            key_fingerprint: key.fingerprint(),
            origin: endpoint.origin().ascii_serialization(),
        };
        match fetch_catalog_http(client, provider, &key).await {
            Ok(models) => {
                self.cache.insert(cache_key, models.clone());
                Ok(models)
            }
            Err(error) => self.cache.get(&cache_key).cloned().ok_or(error),
        }
    }

    pub async fn discover_gemini_http(
        &mut self,
        store: &ProviderCredentialStore,
        client: &reqwest::Client,
    ) -> Result<Vec<CatalogModel>> {
        let provider = ProviderId::Gemini;
        let key = store
            .api_key(provider)?
            .ok_or_else(|| anyhow!("provider credential unavailable"))?;
        let endpoint = Url::parse(catalog_endpoint(provider)).expect("fixed provider catalog URL");
        let cache_key = CatalogCacheKey {
            provider,
            policy: "generate-content-v1beta".to_owned(),
            key_fingerprint: key.fingerprint(),
            origin: endpoint.origin().ascii_serialization(),
        };
        match fetch_catalog_http(client, provider, &key).await {
            Ok(models) => {
                self.cache.insert(cache_key, models.clone());
                Ok(models)
            }
            Err(error) => self.cache.get(&cache_key).cloned().ok_or(error),
        }
    }

    pub async fn discover_gemini_oauth_http(
        &mut self,
        credential: &ProviderOAuthCredential,
        client: &reqwest::Client,
    ) -> Result<Vec<CatalogModel>> {
        if credential.strategy != crate::auth::gemini_oauth::GEMINI_OAUTH_STRATEGY {
            bail!("Gemini OAuth credential strategy mismatch");
        }
        let provider = ProviderId::Gemini;
        let endpoint = Url::parse(catalog_endpoint(provider)).expect("fixed provider catalog URL");
        let cache_key = CatalogCacheKey {
            provider,
            policy: "gemini-oauth-generate-content-v1beta".to_owned(),
            // Bind OAuth cache to the public client/account strategy, not the
            // short-lived access token, so a normal refresh keeps fallback.
            key_fingerprint: blake3::hash(credential.client_id.as_bytes())
                .to_hex()
                .to_string(),
            origin: endpoint.origin().ascii_serialization(),
        };
        match fetch_gemini_catalog_oauth_http_at(client, &credential.access_token, &endpoint).await
        {
            Ok(models) => {
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
        let endpoint = Url::parse(catalog_endpoint(provider)).expect("fixed provider catalog URL");
        let cache_key = registered_cache_key(provider, &key, &endpoint, registered);
        let response = fetch_catalog_http(client, provider, &key).await;
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

async fn fetch_catalog_http(
    client: &reqwest::Client,
    provider: ProviderId,
    key: &ApiKey,
) -> Result<Vec<CatalogModel>> {
    let endpoint = Url::parse(catalog_endpoint(provider)).expect("fixed provider catalog URL");
    fetch_catalog_http_at(client, provider, key, &endpoint).await
}

async fn fetch_catalog_http_at(
    client: &reqwest::Client,
    provider: ProviderId,
    key: &ApiKey,
    endpoint: &Url,
) -> Result<Vec<CatalogModel>> {
    if provider == ProviderId::Gemini {
        return fetch_gemini_catalog_http_at(client, key, endpoint).await;
    }
    if provider != ProviderId::Anthropic {
        let response = client
            .get(endpoint.clone())
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
        return parse_catalog(provider, value);
    }

    let mut models = Vec::new();
    let mut seen_models = HashSet::new();
    let mut after_id: Option<String> = None;
    let mut seen_cursors = std::collections::HashSet::new();
    for _ in 0..100 {
        let mut request = client
            .get(endpoint.clone())
            .header("x-api-key", key.expose())
            .header("anthropic-version", ANTHROPIC_API_VERSION)
            .query(&[("limit", "1000")]);
        if let Some(cursor) = after_id.as_deref() {
            request = request.query(&[("after_id", cursor)]);
        }
        let response = request
            .send()
            .await
            .context("requesting Anthropic model catalog")?
            .error_for_status()
            .context("Anthropic model catalog returned an error status")?;
        let value = response
            .json::<Value>()
            .await
            .context("decoding Anthropic model catalog")?;
        let (page_models, next_cursor) = parse_anthropic_page(value)?;
        for model in page_models {
            if seen_models.insert(model.wire_id.clone()) {
                models.push(model);
            }
        }
        let Some(cursor) = next_cursor else {
            after_id = None;
            break;
        };
        if !seen_cursors.insert(cursor.clone()) {
            bail!("Anthropic catalog repeated pagination cursor");
        }
        after_id = Some(cursor);
    }
    if after_id.is_some() {
        bail!("Anthropic catalog exceeded pagination safety limit");
    }
    Ok(models)
}

async fn fetch_gemini_catalog_http_at(
    client: &reqwest::Client,
    key: &ApiKey,
    endpoint: &Url,
) -> Result<Vec<CatalogModel>> {
    let mut models = Vec::new();
    let mut seen_models = HashSet::new();
    let mut page_token: Option<String> = None;
    let mut seen_tokens = HashSet::new();
    for _ in 0..100 {
        let mut request = client
            .get(endpoint.clone())
            .header("x-goog-api-key", key.expose())
            .query(&[("pageSize", "1000")]);
        if let Some(token) = page_token.as_deref() {
            request = request.query(&[("pageToken", token)]);
        }
        let value = request
            .send()
            .await
            .context("requesting Gemini model catalog")?
            .error_for_status()
            .context("Gemini model catalog returned an error status")?
            .json::<Value>()
            .await
            .context("decoding Gemini model catalog")?;
        let (page_models, next_token) = parse_gemini_page(value)?;
        for model in page_models {
            if seen_models.insert(model.wire_id.clone()) {
                models.push(model);
            }
        }
        let Some(token) = next_token else {
            page_token = None;
            break;
        };
        if !seen_tokens.insert(token.clone()) {
            bail!("Gemini catalog repeated pagination token");
        }
        page_token = Some(token);
    }
    if page_token.is_some() {
        bail!("Gemini catalog exceeded pagination safety limit");
    }
    Ok(models)
}

async fn fetch_gemini_catalog_oauth_http_at(
    client: &reqwest::Client,
    access_token: &str,
    endpoint: &Url,
) -> Result<Vec<CatalogModel>> {
    let mut models = Vec::new();
    let mut seen_models = HashSet::new();
    let mut page_token: Option<String> = None;
    let mut seen_tokens = HashSet::new();
    for _ in 0..100 {
        let mut request = client
            .get(endpoint.clone())
            .bearer_auth(access_token)
            .query(&[("pageSize", "1000")]);
        if let Some(token) = page_token.as_deref() {
            request = request.query(&[("pageToken", token)]);
        }
        let value = request
            .send()
            .await
            .context("requesting Gemini OAuth model catalog")?
            .error_for_status()
            .context("Gemini OAuth model catalog returned an error status")?
            .json::<Value>()
            .await
            .context("decoding Gemini OAuth model catalog")?;
        let (page_models, next_token) = parse_gemini_page(value)?;
        for model in page_models {
            if seen_models.insert(model.wire_id.clone()) {
                models.push(model);
            }
        }
        let Some(token) = next_token else {
            page_token = None;
            break;
        };
        if !seen_tokens.insert(token.clone()) {
            bail!("Gemini OAuth catalog repeated pagination token");
        }
        page_token = Some(token);
    }
    if page_token.is_some() {
        bail!("Gemini OAuth catalog exceeded pagination safety limit");
    }
    Ok(models)
}

fn parse_gemini_page(value: Value) -> Result<(Vec<CatalogModel>, Option<String>)> {
    let raw_models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Gemini catalog has no models array"))?;
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for model in raw_models {
        let supports_generate = model
            .get("supportedGenerationMethods")
            .and_then(Value::as_array)
            .is_some_and(|methods| {
                methods
                    .iter()
                    .any(|method| method.as_str() == Some("generateContent"))
            });
        if !supports_generate {
            continue;
        }
        let Some(wire_id) = model
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| name.strip_prefix("models/"))
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        if seen.insert(wire_id.to_owned()) {
            models.push(CatalogModel {
                id: format!("gemini/{wire_id}"),
                wire_id: wire_id.to_owned(),
                provider: ProviderId::Gemini,
            });
        }
    }
    let next = value
        .get("nextPageToken")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    Ok((models, next))
}

fn parse_anthropic_page(value: Value) -> Result<(Vec<CatalogModel>, Option<String>)> {
    let models = parse_catalog(ProviderId::Anthropic, value.clone())?;
    let has_more = value
        .get("has_more")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("Anthropic catalog page has no has_more boolean"))?;
    if !has_more {
        return Ok((models, None));
    }
    let cursor = value
        .get("last_id")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
        .ok_or_else(|| anyhow!("Anthropic catalog page has_more without last_id"))?;
    Ok((models, Some(cursor.to_owned())))
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
        ProviderId::Anthropic => "https://api.anthropic.com/v1",
        ProviderId::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        ProviderId::Openai => "https://api.openai.com/v1",
        ProviderId::Openrouter => "https://openrouter.ai/api/v1",
    }
}

fn catalog_endpoint(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Anthropic => ANTHROPIC_MODELS_URL,
        ProviderId::Gemini => GEMINI_MODELS_URL,
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
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for model in raw_models {
        let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        if provider == ProviderId::Openrouter && !openrouter_model_is_usable(model) {
            continue;
        }
        if seen.insert(id.to_owned()) {
            models.push(CatalogModel {
                id: format!("{}/{}", provider.as_str(), id),
                wire_id: id.to_owned(),
                provider,
            });
        }
    }
    Ok(models)
}

fn parse_custom_catalog(provider: &str, value: Value) -> Result<Vec<CustomCatalogModel>> {
    let raw_models = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("custom Provider catalog has no data array"))?;
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for model in raw_models {
        let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() || !seen.insert(id.to_owned()) {
            continue;
        }
        models.push(CustomCatalogModel {
            id: format!("{provider}/{id}"),
            wire_id: id.to_owned(),
            provider: provider.to_owned(),
        });
    }
    Ok(models)
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

    #[test]
    fn experimental_oauth_visibility_is_default_hidden_and_independently_gated() {
        let default_methods = provider_auth_methods(ProviderReleaseGates::default(), false, false);
        assert!(
            default_methods
                .iter()
                .all(|method| method.stability == AdapterStability::Stable)
        );

        let codex_only = provider_auth_methods(
            ProviderReleaseGates {
                codex_oauth: true,
                ..Default::default()
            },
            false,
            false,
        );
        let experimental: Vec<_> = codex_only
            .iter()
            .filter(|method| method.stability == AdapterStability::Experimental)
            .collect();
        assert_eq!(experimental.len(), 1);
        assert_eq!(experimental[0].id, "codex_oauth_compat");
        assert!(experimental[0].requires_confirmation);
        assert_eq!(
            experimental[0].availability,
            AdapterAvailability::NotEnabled
        );
        assert!(
            codex_only
                .iter()
                .any(|method| method.id == "openai_api_key"),
            "experimental compatibility must retain the stable API-key fallback"
        );
    }

    #[test]
    fn conditional_adapter_statuses_never_claim_unverified_availability() {
        let methods = provider_auth_methods(
            ProviderReleaseGates {
                gemini_oauth: true,
                github_copilot: true,
                claude_oauth: true,
                ..Default::default()
            },
            false,
            false,
        );
        assert_eq!(
            methods
                .iter()
                .find(|method| method.id == "github_copilot")
                .unwrap()
                .availability,
            AdapterAvailability::BlockedExternal
        );
        assert_eq!(
            methods
                .iter()
                .find(|method| method.id == "gemini_oauth")
                .unwrap()
                .availability,
            AdapterAvailability::NotEnabled
        );
        assert!(
            methods
                .iter()
                .find(|method| method.id == "claude_oauth_compat")
                .unwrap()
                .requires_confirmation
        );

        let ready = provider_auth_methods(
            ProviderReleaseGates {
                gemini_oauth: true,
                ..Default::default()
            },
            false,
            true,
        );
        assert_eq!(
            ready
                .iter()
                .find(|method| method.id == "gemini_oauth")
                .unwrap()
                .availability,
            AdapterAvailability::Available
        );
    }

    #[test]
    fn named_custom_credentials_are_isolated_and_builtin_names_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        store
            .store_custom_api_key("custom-a", key("custom-a-secret"))
            .unwrap();
        store
            .store_custom_api_key("custom-b", key("custom-b-secret"))
            .unwrap();

        assert_eq!(
            store.custom_api_key("custom-a").unwrap().unwrap().masked(),
            "****cret"
        );
        assert_eq!(
            store.custom_api_key("custom-b").unwrap().unwrap().masked(),
            "****cret"
        );
        let root: Value =
            serde_json::from_slice(&fs::read(directory.path().join("auth.json")).unwrap()).unwrap();
        assert_eq!(
            root.pointer("/providers/custom-a/credentials/custom-a/type")
                .and_then(Value::as_str),
            Some("api_key")
        );
        assert!(
            store
                .store_custom_api_key("openai", key("must-not-write"))
                .is_err()
        );
    }

    #[test]
    fn custom_catalogs_use_same_origin_and_isolated_failure_cache() {
        let mut registry = ProviderAdapterRegistry::default();
        let a = registry
            .discover_custom_with(
                "custom-a",
                "https://a.example/api/",
                Some("/v1/models"),
                "authorization",
                key("a-secret"),
                |request| {
                    assert_eq!(request.endpoint.as_str(), "https://a.example/v1/models");
                    assert_eq!(
                        request.credential_header(),
                        ("authorization", "Bearer a-secret")
                    );
                    Ok(json!({ "data": [{ "id": "alpha" }] }))
                },
            )
            .unwrap();
        let b = registry
            .discover_custom_with(
                "custom-b",
                "https://b.example/v1",
                None,
                "x-api-key",
                key("b-secret"),
                |_| Ok(json!({ "data": [{ "id": "beta" }] })),
            )
            .unwrap();
        assert_eq!(a[0].id, "custom-a/alpha");
        assert_eq!(b[0].id, "custom-b/beta");

        let cached_a = registry
            .discover_custom_with(
                "custom-a",
                "https://a.example/api/",
                Some("/v1/models"),
                "authorization",
                key("a-secret"),
                |_| bail!("temporary failure"),
            )
            .unwrap();
        assert_eq!(cached_a, a);
        assert!(
            registry
                .discover_custom_with(
                    "custom-a",
                    "https://a.example/api/",
                    Some("https://evil.example/models"),
                    "authorization",
                    key("a-secret"),
                    |_| unreachable!(),
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn custom_catalog_http_uses_named_credential_and_openai_shape() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let upstream = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer custom-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{ "id": "remote-a" }, { "id": "remote-b" }]
            })))
            .expect(1)
            .mount(&upstream)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        store
            .store_custom_api_key("custom-a", key("custom-secret"))
            .unwrap();

        let models = ProviderAdapterRegistry::default()
            .discover_custom_http(
                "custom-a",
                &upstream.uri(),
                None,
                "authorization",
                &store,
                &reqwest::Client::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["custom-a/remote-a", "custom-a/remote-b"]
        );
    }

    #[tokio::test]
    async fn gemini_oauth_catalog_uses_bearer_and_native_pagination() {
        use wiremock::matchers::{header, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1beta/models"))
            .and(header("authorization", "Bearer oauth-access"))
            .and(query_param("pageSize", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [{
                    "name": "models/gemini-oauth-model",
                    "supportedGenerationMethods": ["generateContent"]
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let endpoint = Url::parse(&format!("{}/v1beta/models", server.uri())).unwrap();
        let models =
            fetch_gemini_catalog_oauth_http_at(&reqwest::Client::new(), "oauth-access", &endpoint)
                .await
                .unwrap();
        assert_eq!(models[0].id, "gemini/gemini-oauth-model");
    }

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
        assert!(openai.credential_header().1.contains("openai-secret"));
        assert!(!openai.credential_header().1.contains("router-secret"));
        assert!(router.credential_header().1.contains("router-secret"));
        assert!(!router.credential_header().1.contains("openai-secret"));
    }

    #[test]
    fn anthropic_projection_uses_fixed_origin_and_x_api_key() {
        let registry = ProviderAdapterRegistry::default();
        let request = registry
            .request_projection(
                ProviderId::Anthropic,
                "claude-sonnet",
                key("anthropic-secret"),
            )
            .unwrap();
        assert_eq!(request.endpoint.as_str(), "https://api.anthropic.com/v1");
        assert_eq!(request.model_id, "claude-sonnet");
        assert_eq!(
            request.credential_header(),
            ("x-api-key", "anthropic-secret")
        );
    }

    #[test]
    fn anthropic_pagination_uses_last_id_and_rejects_missing_cursor() {
        let (first, cursor) = parse_anthropic_page(json!({
            "data": [{"id": "claude-new"}],
            "has_more": true,
            "first_id": "claude-new",
            "last_id": "opaque-cursor"
        }))
        .unwrap();
        assert_eq!(first[0].id, "anthropic/claude-new");
        assert_eq!(cursor.as_deref(), Some("opaque-cursor"));

        let (last, cursor) = parse_anthropic_page(json!({
            "data": [{"id": "claude-old"}],
            "has_more": false,
            "last_id": "claude-old"
        }))
        .unwrap();
        assert_eq!(last[0].id, "anthropic/claude-old");
        assert!(cursor.is_none());

        assert!(
            parse_anthropic_page(json!({"data": [], "has_more": true})).is_err(),
            "has_more without the official last_id cursor must fail closed"
        );
    }

    #[tokio::test]
    async fn anthropic_http_catalog_sends_headers_and_walks_official_cursor() {
        use std::sync::{Arc, Mutex};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let seen_queries = Arc::new(Mutex::new(Vec::new()));
        let captured = seen_queries.clone();
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(move |request: &wiremock::Request| {
                assert_eq!(
                    request
                        .headers
                        .get("x-api-key")
                        .and_then(|value| value.to_str().ok()),
                    Some("anthropic-secret")
                );
                assert_eq!(
                    request
                        .headers
                        .get("anthropic-version")
                        .and_then(|value| value.to_str().ok()),
                    Some(ANTHROPIC_API_VERSION)
                );
                let query = request.url.query().unwrap_or_default().to_owned();
                captured.lock().unwrap().push(query.clone());
                if query.contains("after_id=page-one-cursor") {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "data": [{"id": "claude-old"}],
                        "has_more": false,
                        "last_id": "claude-old"
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "data": [{"id": "claude-new"}],
                        "has_more": true,
                        "last_id": "page-one-cursor"
                    }))
                }
            })
            .expect(2)
            .mount(&server)
            .await;

        let endpoint = Url::parse(&format!("{}/v1/models", server.uri())).unwrap();
        let models = fetch_catalog_http_at(
            &reqwest::Client::new(),
            ProviderId::Anthropic,
            &key("anthropic-secret"),
            &endpoint,
        )
        .await
        .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["anthropic/claude-new", "anthropic/claude-old"]
        );
        let queries = seen_queries.lock().unwrap();
        assert_eq!(queries.len(), 2);
        assert!(queries[0].contains("limit=1000"));
        assert!(!queries[0].contains("after_id"));
        assert!(queries[1].contains("after_id=page-one-cursor"));
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
    fn anthropic_sampling_credentials_use_x_api_key_and_fixed_origin() {
        let directory = tempfile::tempdir().unwrap();
        ProviderCredentialStore::new(directory.path())
            .store_api_key(ProviderId::Anthropic, key("stored-anthropic-secret"))
            .unwrap();
        let mut info = crate::agent::config::ModelInfo::fallback("claude-test");
        info.id = Some("anthropic/claude-test".to_owned());
        info.base_url = "https://attacker.invalid/v1".to_owned();
        let model = crate::agent::config::ModelEntry {
            info,
            api_key: Some("config-secret".to_owned()),
            env_key: None,
            api_base_url: Some("https://attacker.invalid/v1".to_owned()),
        };

        let credentials =
            crate::agent::config::resolve_fixed_provider_credentials_at(&model, directory.path())
                .unwrap();
        assert_eq!(
            credentials.api_key.as_deref(),
            Some("stored-anthropic-secret")
        );
        assert_eq!(credentials.base_url, "https://api.anthropic.com/v1");
        assert_eq!(
            credentials.auth_scheme,
            xai_grok_sampler::config::AuthScheme::XApiKey
        );
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

    #[test]
    fn anthropic_failure_preserves_only_anthropic_cache() {
        let mut registry = ProviderAdapterRegistry::default();
        let anthropic_key = key("anthropic-secret");
        let openai_key = key("openai-secret");
        registry
            .discover_with(ProviderId::Anthropic, anthropic_key.clone(), |_| {
                Ok(json!({"data":[{"id":"claude-test"}]}))
            })
            .unwrap();
        registry
            .discover_with(ProviderId::Openai, openai_key.clone(), |_| {
                Ok(json!({"data":[{"id":"gpt-test"}]}))
            })
            .unwrap();

        let preserved = registry
            .discover_with(ProviderId::Anthropic, anthropic_key.clone(), |_| {
                Err(anyhow!("anthropic offline"))
            })
            .unwrap();
        assert_eq!(preserved[0].id, "anthropic/claude-test");
        assert_eq!(
            registry
                .cached_models(ProviderId::Openai, &openai_key)
                .unwrap()[0]
                .id,
            "openai/gpt-test"
        );
    }

    #[test]
    fn gemini_catalog_strips_resource_prefix_and_filters_generation_method() {
        let (models, next) = parse_gemini_page(json!({
            "models": [
                {"name": "models/gemini-test", "supportedGenerationMethods": ["generateContent"]},
                {"name": "models/embed-test", "supportedGenerationMethods": ["embedContent"]},
                {"name": "bad-prefix", "supportedGenerationMethods": ["generateContent"]}
            ],
            "nextPageToken": "opaque-token"
        }))
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini/gemini-test");
        assert_eq!(models[0].wire_id, "gemini-test");
        assert_eq!(next.as_deref(), Some("opaque-token"));
    }

    #[tokio::test]
    async fn gemini_http_catalog_sends_key_and_walks_page_token() {
        use std::sync::{Arc, Mutex};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let queries = Arc::new(Mutex::new(Vec::new()));
        let captured = queries.clone();
        Mock::given(method("GET"))
            .and(path("/v1beta/models"))
            .respond_with(move |request: &wiremock::Request| {
                assert_eq!(
                    request
                        .headers
                        .get("x-goog-api-key")
                        .and_then(|value| value.to_str().ok()),
                    Some("gemini-secret")
                );
                let query = request.url.query().unwrap_or_default().to_owned();
                captured.lock().unwrap().push(query.clone());
                if query.contains("pageToken=page-one") {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "models": [{
                            "name": "models/gemini-old",
                            "supportedGenerationMethods": ["generateContent"]
                        }]
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "models": [{
                            "name": "models/gemini-new",
                            "supportedGenerationMethods": ["generateContent"]
                        }],
                        "nextPageToken": "page-one"
                    }))
                }
            })
            .expect(2)
            .mount(&server)
            .await;
        let endpoint = Url::parse(&format!("{}/v1beta/models", server.uri())).unwrap();
        let models =
            fetch_gemini_catalog_http_at(&reqwest::Client::new(), &key("gemini-secret"), &endpoint)
                .await
                .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["gemini/gemini-new", "gemini/gemini-old"]
        );
        let queries = queries.lock().unwrap();
        assert!(queries[0].contains("pageSize=1000"));
        assert!(queries[1].contains("pageToken=page-one"));
    }

    fn oauth_credential(
        strategy: &str,
        access_token: &str,
        refresh_token: &str,
    ) -> ProviderOAuthCredential {
        ProviderOAuthCredential {
            strategy: strategy.to_owned(),
            access_token: access_token.to_owned(),
            refresh_token: Some(refresh_token.to_owned()),
            expires_at: Some(1),
            token_endpoint: "http://127.0.0.1/token".to_owned(),
            client_id: "test-client".to_owned(),
            source: Some("test".to_owned()),
        }
    }

    #[test]
    fn provider_oauth_refresh_is_cross_process_singleflight() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let store = ProviderCredentialStore::with_path(&path);
        let initial = oauth_credential("codex_oauth", "access-old", "refresh-old");
        let observed = initial.refresh_fingerprint().unwrap();
        store.store_oauth(ProviderId::Openai, initial).unwrap();

        let barrier = Arc::new(Barrier::new(3));
        let refresh_count = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let barrier = barrier.clone();
            let refresh_count = refresh_count.clone();
            let path = path.clone();
            let observed = observed.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                ProviderCredentialStore::with_path(path)
                    .refresh_oauth_with(ProviderId::Openai, "codex_oauth", Some(&observed), |_| {
                        refresh_count.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(100));
                        Ok(oauth_credential("codex_oauth", "access-new", "refresh-new"))
                    })
                    .unwrap()
            }));
        }
        barrier.wait();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(refresh_count.load(Ordering::SeqCst), 1);
        assert!(
            results
                .iter()
                .all(|result| result.access_token == "access-new")
        );
        let final_value = store
            .oauth(ProviderId::Openai, "codex_oauth")
            .unwrap()
            .unwrap();
        assert_eq!(final_value.refresh_token.as_deref(), Some("refresh-new"));
    }

    #[test]
    fn clearing_failed_oauth_preserves_api_key_and_other_provider() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        store
            .store_api_key(ProviderId::Openai, key("openai-key"))
            .unwrap();
        store
            .store_oauth(
                ProviderId::Openai,
                oauth_credential("codex_oauth", "access", "refresh"),
            )
            .unwrap();
        store
            .store_oauth(
                ProviderId::Anthropic,
                oauth_credential("claude_oauth", "access-a", "refresh-a"),
            )
            .unwrap();
        assert!(
            store
                .clear_oauth(ProviderId::Openai, "codex_oauth")
                .unwrap()
        );
        assert_eq!(
            store.api_key(ProviderId::Openai).unwrap().unwrap().masked(),
            "****-key"
        );
        assert!(
            store
                .oauth(ProviderId::Anthropic, "claude_oauth")
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn provider_oauth_http_refresh_rotates_atomically() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=refresh-old"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access-new",
                "refresh_token": "refresh-new",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        let mut initial = oauth_credential("gemini_oauth", "access-old", "refresh-old");
        initial.token_endpoint = format!("{}/token", server.uri());
        let observed = initial.refresh_fingerprint().unwrap();
        store.store_oauth(ProviderId::Gemini, initial).unwrap();
        let refreshed = store
            .refresh_oauth_http(
                ProviderId::Gemini,
                "gemini_oauth",
                Some(&observed),
                &reqwest::Client::new(),
            )
            .await
            .unwrap();
        assert_eq!(refreshed.access_token, "access-new");
        assert_eq!(refreshed.refresh_token.as_deref(), Some("refresh-new"));
        assert!(
            store
                .oauth(ProviderId::Gemini, "gemini_oauth")
                .unwrap()
                .unwrap()
                .expires_at
                .is_some()
        );
    }

    #[tokio::test]
    async fn terminal_oauth_refresh_removes_only_target_strategy() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        let mut target = oauth_credential("codex_oauth", "access", "refresh");
        target.token_endpoint = format!("{}/token", server.uri());
        let observed = target.refresh_fingerprint().unwrap();
        store.store_oauth(ProviderId::Openai, target).unwrap();
        store
            .store_api_key(ProviderId::Openai, key("stable-openai-key"))
            .unwrap();
        store
            .store_oauth(
                ProviderId::Anthropic,
                oauth_credential("claude_oauth", "access-a", "refresh-a"),
            )
            .unwrap();
        assert!(
            store
                .refresh_oauth_http(
                    ProviderId::Openai,
                    "codex_oauth",
                    Some(&observed),
                    &reqwest::Client::new(),
                )
                .await
                .is_err()
        );
        assert!(
            store
                .oauth(ProviderId::Openai, "codex_oauth")
                .unwrap()
                .is_none()
        );
        assert!(store.api_key(ProviderId::Openai).unwrap().is_some());
        assert!(
            store
                .oauth(ProviderId::Anthropic, "claude_oauth")
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn transient_oauth_refresh_preserves_target_credential() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(503)
                    .set_body_json(json!({"error": "temporarily_unavailable"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let store = ProviderCredentialStore::new(directory.path());
        let mut initial = oauth_credential("gemini_oauth", "access-old", "refresh-old");
        initial.token_endpoint = format!("{}/token", server.uri());
        let observed = initial.refresh_fingerprint().unwrap();
        store.store_oauth(ProviderId::Gemini, initial).unwrap();
        assert!(
            store
                .refresh_oauth_http(
                    ProviderId::Gemini,
                    "gemini_oauth",
                    Some(&observed),
                    &reqwest::Client::new(),
                )
                .await
                .is_err()
        );
        let preserved = store
            .oauth(ProviderId::Gemini, "gemini_oauth")
            .unwrap()
            .unwrap();
        assert_eq!(preserved.access_token, "access-old");
        assert_eq!(preserved.refresh_token.as_deref(), Some("refresh-old"));
    }

    #[test]
    fn provider_oauth_debug_is_secret_free() {
        let credential =
            oauth_credential("codex_oauth", "access-super-secret", "refresh-super-secret");
        let debug = format!("{credential:?}");
        assert!(!debug.contains("access-super-secret"));
        assert!(!debug.contains("refresh-super-secret"));
        assert!(debug.contains("has_access_token"));
        assert!(debug.contains("has_refresh_token"));
    }
}
