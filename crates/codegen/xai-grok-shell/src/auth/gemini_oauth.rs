//! User-owned Google OAuth client support for the Gemini API.
//!
//! The client secret remains in the user-supplied Google client JSON. Only
//! tokens, the public client id, and the source path are stored in auth.json.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use rand::RngCore as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use url::Url;

use super::provider_registry::{ProviderCredentialStore, ProviderId, ProviderOAuthCredential};

pub const GEMINI_OAUTH_STRATEGY: &str = "gemini_user_oauth";
pub const GEMINI_OAUTH_SCOPE: &str =
    "https://www.googleapis.com/auth/generative-language.retriever";

#[derive(Clone)]
pub struct GeminiOAuthClient {
    pub client_id: String,
    client_secret: String,
    pub auth_uri: Url,
    pub token_uri: Url,
    pub source: PathBuf,
}

impl std::fmt::Debug for GeminiOAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiOAuthClient")
            .field("client_id", &self.client_id)
            .field("has_client_secret", &!self.client_secret.is_empty())
            .field("auth_uri", &self.auth_uri)
            .field("token_uri", &self.token_uri)
            .field("source", &self.source)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct GoogleClientFile {
    installed: Option<GoogleClientSection>,
    web: Option<GoogleClientSection>,
}

#[derive(Debug, Deserialize)]
struct GoogleClientSection {
    client_id: String,
    client_secret: String,
    auth_uri: String,
    token_uri: String,
    #[serde(default)]
    redirect_uris: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeminiOAuthStart {
    pub authorization_url: Url,
    pub state: String,
    pub pkce_verifier: String,
    pub redirect_uri: Url,
}

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

impl GeminiOAuthClient {
    pub fn from_client_file(path: impl AsRef<Path>) -> Result<Self> {
        let source = path.as_ref().to_path_buf();
        let bytes = std::fs::read(&source).context("reading Google OAuth client file")?;
        let file: GoogleClientFile =
            serde_json::from_slice(&bytes).context("invalid Google OAuth client JSON")?;
        let section = file
            .installed
            .or(file.web)
            .ok_or_else(|| anyhow!("Google OAuth client JSON has no installed or web client"))?;
        if section.client_id.trim().is_empty() || section.client_secret.trim().is_empty() {
            bail!("Google OAuth client id or secret is empty");
        }
        let auth_uri = Url::parse(&section.auth_uri).context("invalid Google authorization URI")?;
        let token_uri = Url::parse(&section.token_uri).context("invalid Google token URI")?;
        if auth_uri.scheme() != "https" || token_uri.scheme() != "https" {
            bail!("Google OAuth endpoints must use HTTPS");
        }
        if !section.redirect_uris.is_empty()
            && !section.redirect_uris.iter().any(|uri| {
                uri.starts_with("http://127.0.0.1")
                    || uri.starts_with("http://localhost")
                    || uri == "urn:ietf:wg:oauth:2.0:oob"
            })
        {
            bail!("Google OAuth client has no desktop-compatible redirect URI");
        }
        Ok(Self {
            client_id: section.client_id,
            client_secret: section.client_secret,
            auth_uri,
            token_uri,
            source,
        })
    }

    pub fn begin(&self, redirect_uri: Url) -> Result<GeminiOAuthStart> {
        if redirect_uri.scheme() != "http"
            || !matches!(
                redirect_uri.host_str(),
                Some("127.0.0.1") | Some("localhost")
            )
        {
            bail!("Gemini OAuth redirect must use loopback HTTP");
        }
        let state = random_urlsafe(32);
        let pkce_verifier = random_urlsafe(64);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(pkce_verifier.as_bytes()));
        let mut authorization_url = self.auth_uri.clone();
        authorization_url
            .query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri.as_str())
            .append_pair("response_type", "code")
            .append_pair("scope", GEMINI_OAUTH_SCOPE)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(GeminiOAuthStart {
            authorization_url,
            state,
            pkce_verifier,
            redirect_uri,
        })
    }

    pub async fn exchange_and_store(
        &self,
        client: &reqwest::Client,
        store: &ProviderCredentialStore,
        start: &GeminiOAuthStart,
        returned_state: &str,
        code: &str,
    ) -> Result<ProviderOAuthCredential> {
        if returned_state != start.state {
            bail!("Gemini OAuth state mismatch");
        }
        if code.trim().is_empty() {
            bail!("Gemini OAuth authorization code is empty");
        }
        let response = client
            .post(self.token_uri.clone())
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code),
                ("code_verifier", start.pkce_verifier.as_str()),
                ("grant_type", "authorization_code"),
                ("redirect_uri", start.redirect_uri.as_str()),
            ])
            .send()
            .await
            .context("Gemini OAuth token exchange failed")?;
        if !response.status().is_success() {
            bail!("Gemini OAuth token exchange was rejected");
        }
        let token: GoogleTokenResponse = response
            .json()
            .await
            .context("invalid Gemini OAuth token response")?;
        let credential = ProviderOAuthCredential {
            strategy: GEMINI_OAUTH_STRATEGY.to_owned(),
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at: token.expires_in.map(|ttl| unix_now().saturating_add(ttl)),
            token_endpoint: self.token_uri.as_str().to_owned(),
            client_id: self.client_id.clone(),
            source: Some(self.source.to_string_lossy().into_owned()),
        };
        store.store_oauth(ProviderId::Gemini, credential.clone())?;
        Ok(credential)
    }

    pub async fn valid_credential(
        &self,
        client: &reqwest::Client,
        store: &ProviderCredentialStore,
    ) -> Result<ProviderOAuthCredential> {
        let credential = store
            .oauth(ProviderId::Gemini, GEMINI_OAUTH_STRATEGY)?
            .ok_or_else(|| anyhow!("Gemini OAuth credential unavailable"))?;
        if !credential.is_expired_or_near_expiry() {
            return Ok(credential);
        }
        let observed = credential.refresh_fingerprint();
        store
            .refresh_oauth_http_with_client_secret(
                ProviderId::Gemini,
                GEMINI_OAUTH_STRATEGY,
                observed.as_deref(),
                client,
                self.client_secret(),
            )
            .await
    }

    pub(crate) fn client_secret(&self) -> &str {
        &self.client_secret
    }
}

pub async fn valid_credential_from_effective_config(
    client: &reqwest::Client,
) -> Result<ProviderOAuthCredential> {
    let raw = crate::config::load_effective_config()
        .context("loading config for Gemini OAuth refresh")?;
    let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw)
        .map_err(|error| anyhow!("parsing config for Gemini OAuth refresh: {error}"))?;
    if !cfg.features.provider_gemini_oauth.unwrap_or(false) {
        bail!("Gemini OAuth is not enabled");
    }
    let client_file = cfg
        .providers
        .get("gemini")
        .filter(|provider| {
            provider.auth_strategy == Some(crate::agent::config::ProviderAuthStrategy::GeminiOauth)
        })
        .and_then(|provider| provider.oauth_client_file.as_deref())
        .ok_or_else(|| anyhow!("Gemini OAuth client file is not configured"))?;
    let oauth_client = GeminiOAuthClient::from_client_file(client_file)?;
    let store = ProviderCredentialStore::new(crate::util::grok_home::grok_home());
    oauth_client.valid_credential(client, &store).await
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_fixture(path: &Path, token_uri: &str) {
        std::fs::write(
            path,
            serde_json::json!({
                "installed": {
                    "client_id": "user-client.apps.googleusercontent.com",
                    "client_secret": "client-secret-must-not-leak",
                    "auth_uri": "https://accounts.google.com/o/oauth2/v2/auth",
                    "token_uri": token_uri,
                    "redirect_uris": ["http://localhost"]
                }
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn tc12_missing_or_invalid_client_fails_explainably() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            GeminiOAuthClient::from_client_file(directory.path().join("missing.json")).is_err()
        );
        let invalid = directory.path().join("invalid.json");
        std::fs::write(&invalid, "{}").unwrap();
        assert!(GeminiOAuthClient::from_client_file(invalid).is_err());
    }

    #[test]
    fn tc12_valid_client_builds_pkce_authorization_url_without_secret() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        client_fixture(&path, "https://oauth2.googleapis.com/token");
        let client = GeminiOAuthClient::from_client_file(&path).unwrap();
        let start = client
            .begin(Url::parse("http://127.0.0.1:43123/callback").unwrap())
            .unwrap();
        let rendered = start.authorization_url.as_str();
        assert!(rendered.contains("code_challenge="));
        assert!(rendered.contains("access_type=offline"));
        assert!(rendered.contains("generative-language.retriever"));
        assert!(!rendered.contains("client-secret-must-not-leak"));
        assert!(!format!("{client:?}").contains("client-secret-must-not-leak"));
    }

    #[tokio::test]
    async fn token_exchange_stores_tokens_but_not_client_secret() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(
                "client_secret=client-secret-must-not-leak",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "gemini-access",
                "refresh_token": "gemini-refresh",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.json");
        // Production validation requires HTTPS. The injected test endpoint is
        // patched after loading a valid client fixture.
        client_fixture(&path, "https://oauth2.googleapis.com/token");
        let mut oauth = GeminiOAuthClient::from_client_file(&path).unwrap();
        oauth.token_uri = Url::parse(&format!("{}/token", server.uri())).unwrap();
        let start = oauth
            .begin(Url::parse("http://127.0.0.1:43123/callback").unwrap())
            .unwrap();
        let store = ProviderCredentialStore::new(directory.path().join("grok"));
        oauth
            .exchange_and_store(
                &reqwest::Client::new(),
                &store,
                &start,
                &start.state,
                "authorization-code",
            )
            .await
            .unwrap();
        let raw = std::fs::read_to_string(directory.path().join("grok/auth.json")).unwrap();
        assert!(raw.contains("gemini-access"));
        assert!(raw.contains("gemini-refresh"));
        assert!(!raw.contains("client-secret-must-not-leak"));
    }
}
