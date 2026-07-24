//! GitHub Copilot integration through the official SDK and CLI runtime.
//!
//! Authentication remains owned by the Copilot CLI. This module never accepts,
//! reads, copies, or persists a GitHub token.

use async_trait::async_trait;
use github_copilot_sdk::{
    Client, ClientOptions, SessionConfig, session_events::AssistantMessageData,
};
use serde::Serialize;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotModel {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotCompletion {
    pub content: String,
    pub model: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopilotFailureKind {
    BlockedExternal,
    AuthenticationRequired,
    IncompatibleRuntime,
    RuntimeFailure,
}

#[derive(Debug, Error)]
#[error("GitHub Copilot is unavailable ({kind:?})")]
pub struct CopilotSdkError {
    pub kind: CopilotFailureKind,
}

impl CopilotSdkError {
    fn from_sdk(error: &impl std::fmt::Display) -> Self {
        let message = error.to_string().to_ascii_lowercase();
        let kind = if message.contains("auth")
            || message.contains("login")
            || message.contains("unauthorized")
            || message.contains("forbidden")
        {
            CopilotFailureKind::AuthenticationRequired
        } else if message.contains("version mismatch")
            || message.contains("protocol version")
            || message.contains("incompatible")
        {
            CopilotFailureKind::IncompatibleRuntime
        } else if message.contains("not bundled")
            || message.contains("not found")
            || message.contains("no such file")
            || message.contains("startup")
        {
            CopilotFailureKind::BlockedExternal
        } else {
            CopilotFailureKind::RuntimeFailure
        };
        Self { kind }
    }
}

#[async_trait]
pub trait CopilotRuntime: Send + Sync {
    async fn list_models(&self) -> Result<Vec<CopilotModel>, CopilotSdkError>;

    async fn send_and_wait(
        &self,
        model: &str,
        prompt: &str,
    ) -> Result<CopilotCompletion, CopilotSdkError>;
}

/// Production boundary backed exclusively by `github-copilot-sdk`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OfficialCopilotRuntime;

impl OfficialCopilotRuntime {
    async fn start_client() -> Result<Client, CopilotSdkError> {
        Client::start(ClientOptions::default().with_use_logged_in_user(true))
            .await
            .map_err(|error| CopilotSdkError::from_sdk(&error))
    }
}

#[async_trait]
impl CopilotRuntime for OfficialCopilotRuntime {
    async fn list_models(&self) -> Result<Vec<CopilotModel>, CopilotSdkError> {
        let client = Self::start_client().await?;
        let result = client
            .list_models()
            .await
            .map(|models| {
                models
                    .into_iter()
                    .map(|model| CopilotModel {
                        id: model.id,
                        name: model.name,
                    })
                    .collect()
            })
            .map_err(|error| CopilotSdkError::from_sdk(&error));
        let _ = client.stop().await;
        result
    }

    async fn send_and_wait(
        &self,
        model: &str,
        prompt: &str,
    ) -> Result<CopilotCompletion, CopilotSdkError> {
        let client = Self::start_client().await?;
        let config = SessionConfig::default()
            .with_model(model)
            .with_client_name("grok-build")
            .with_available_tools(Vec::<String>::new());
        let result = async {
            let session = client
                .create_session(config)
                .await
                .map_err(|error| CopilotSdkError::from_sdk(&error))?;
            let event = session
                .send_and_wait(prompt)
                .await
                .map_err(|error| CopilotSdkError::from_sdk(&error))?
                .ok_or(CopilotSdkError {
                    kind: CopilotFailureKind::RuntimeFailure,
                })?;
            let message: AssistantMessageData =
                serde_json::from_value(event.data).map_err(|_| CopilotSdkError {
                    kind: CopilotFailureKind::RuntimeFailure,
                })?;
            Ok(CopilotCompletion {
                content: message.content,
                model: message.model,
            })
        }
        .await;
        let _ = client.stop().await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeRuntime {
        requests: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl CopilotRuntime for FakeRuntime {
        async fn list_models(&self) -> Result<Vec<CopilotModel>, CopilotSdkError> {
            Ok(vec![CopilotModel {
                id: "gpt-test".into(),
                name: "GPT Test".into(),
            }])
        }

        async fn send_and_wait(
            &self,
            model: &str,
            prompt: &str,
        ) -> Result<CopilotCompletion, CopilotSdkError> {
            self.requests
                .lock()
                .expect("request lock")
                .push((model.into(), prompt.into()));
            Ok(CopilotCompletion {
                content: "done".into(),
                model: Some(model.into()),
            })
        }
    }

    #[tokio::test]
    async fn discovers_models_through_runtime_boundary() {
        let runtime = FakeRuntime::default();
        assert_eq!(
            runtime.list_models().await.expect("models"),
            vec![CopilotModel {
                id: "gpt-test".into(),
                name: "GPT Test".into(),
            }]
        );
    }

    #[tokio::test]
    async fn routes_session_request_through_runtime_boundary() {
        let runtime = FakeRuntime::default();
        let completion = runtime
            .send_and_wait("gpt-test", "hello")
            .await
            .expect("completion");
        assert_eq!(completion.content, "done");
        assert_eq!(
            *runtime.requests.lock().expect("request lock"),
            vec![("gpt-test".into(), "hello".into())]
        );
    }

    #[test]
    fn public_error_is_redacted_and_structured() {
        let error = CopilotSdkError::from_sdk(&"authorization token=secret");
        assert_eq!(error.kind, CopilotFailureKind::AuthenticationRequired);
        assert!(!error.to_string().contains("secret"));
    }
}
