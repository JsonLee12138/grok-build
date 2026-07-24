//! `/provider <provider-name>` -- configure built-in or named Custom authentication.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct ProviderCommand;

impl SlashCommand for ProviderCommand {
    fn name(&self) -> &str {
        "provider"
    }

    fn description(&self) -> &str {
        "Select an authentication provider"
    }

    fn usage(&self) -> &str {
        "/provider [provider]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let method = args.trim();
        if let Some(method_id) = method.strip_prefix("method:") {
            return CommandResult::Action(Action::SelectProviderMethod {
                method_id: method_id.to_owned(),
                confirmed: false,
            });
        }
        if let Some(method_id) = method.strip_prefix("confirm:") {
            return CommandResult::Action(Action::SelectProviderMethod {
                method_id: method_id.to_owned(),
                confirmed: true,
            });
        }
        match method {
            "" => CommandResult::Action(Action::LoadProviderMethods),
            "xai" => CommandResult::Action(Action::SelectXaiProvider),
            "anthropic" | "gemini" | "openai" | "openrouter" => {
                CommandResult::Action(Action::OpenProviderApiKey(args.trim().to_owned()))
            }
            "gemini-oauth" => CommandResult::Action(Action::SelectProviderMethod {
                method_id: "gemini_oauth".to_owned(),
                confirmed: false,
            }),
            "github-copilot" => CommandResult::Action(Action::SelectProviderMethod {
                method_id: "github_copilot".to_owned(),
                confirmed: false,
            }),
            "github_copilot" | "codex_oauth_compat" | "claude_oauth_compat" | "gemini_oauth" => {
                CommandResult::Action(Action::SelectProviderMethod {
                    method_id: method.to_owned(),
                    confirmed: false,
                })
            }
            "codex-oauth" => CommandResult::Action(Action::SelectProviderMethod {
                method_id: "codex_oauth_compat".to_owned(),
                confirmed: false,
            }),
            "claude-oauth" => CommandResult::Action(Action::SelectProviderMethod {
                method_id: "claude_oauth_compat".to_owned(),
                confirmed: false,
            }),
            "cancel" => CommandResult::HandledNoOp,
            provider if valid_custom_provider_name(provider) => {
                CommandResult::Action(Action::OpenProviderApiKey(provider.to_owned()))
            }
            provider => CommandResult::Error(format!("Invalid provider name `{provider}`")),
        }
    }
}

fn valid_custom_provider_name(provider: &str) -> bool {
    !provider.is_empty()
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xai_selects_the_xai_provider() {
        let command = ProviderCommand;
        let models = crate::acp::model_state::ModelState::default();
        let bundle = crate::app::bundle::BundleState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &bundle,
            screen_mode: crate::app::ScreenMode::Fullscreen,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        };

        assert!(matches!(
            command.run(&mut ctx, "gemini"),
            CommandResult::Action(Action::OpenProviderApiKey(provider))
                if provider == "gemini"
        ));
        assert!(matches!(
            command.run(&mut ctx, "xai"),
            CommandResult::Action(Action::SelectXaiProvider)
        ));
    }

    #[test]
    fn rejects_unknown_provider_without_mutating_active_selection() {
        let command = ProviderCommand;
        let models = crate::acp::model_state::ModelState::default();
        let bundle = crate::app::bundle::BundleState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &bundle,
            screen_mode: crate::app::ScreenMode::Fullscreen,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        };

        assert!(matches!(
            command.run(&mut ctx, "bad/name"),
            CommandResult::Error(_)
        ));
    }

    #[test]
    fn api_key_providers_open_masked_entry() {
        let command = ProviderCommand;
        let models = crate::acp::model_state::ModelState::default();
        let bundle = crate::app::bundle::BundleState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &bundle,
            screen_mode: crate::app::ScreenMode::Fullscreen,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        };

        assert!(matches!(
            command.run(&mut ctx, "anthropic"),
            CommandResult::Action(Action::OpenProviderApiKey(provider))
                if provider == "anthropic"
        ));
        assert!(matches!(
            command.run(&mut ctx, "openai"),
            CommandResult::Action(Action::OpenProviderApiKey(provider))
                if provider == "openai"
        ));
        assert!(matches!(
            command.run(&mut ctx, "openrouter"),
            CommandResult::Action(Action::OpenProviderApiKey(provider))
                if provider == "openrouter"
        ));
        assert!(matches!(
            command.run(&mut ctx, "custom-a"),
            CommandResult::Action(Action::OpenProviderApiKey(provider))
                if provider == "custom-a"
        ));
        assert!(matches!(
            command.run(&mut ctx, "gemini-oauth"),
            CommandResult::Action(Action::SelectProviderMethod {
                method_id,
                confirmed: false
            }) if method_id == "gemini_oauth"
        ));
    }

    #[test]
    fn empty_provider_loads_gate_aware_methods() {
        let command = ProviderCommand;
        let models = crate::acp::model_state::ModelState::default();
        let bundle = crate::app::bundle::BundleState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &bundle,
            screen_mode: crate::app::ScreenMode::Fullscreen,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        };

        assert!(matches!(
            command.run(&mut ctx, ""),
            CommandResult::Action(Action::LoadProviderMethods)
        ));
        assert!(matches!(
            command.run(&mut ctx, "codex_oauth_compat"),
            CommandResult::Action(Action::SelectProviderMethod {
                method_id,
                confirmed: false
            }) if method_id == "codex_oauth_compat"
        ));
    }
}
