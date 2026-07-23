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
        "/provider <xai|anthropic|gemini|openai|openrouter|custom-name>"
    }

    fn args_required(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        match args.trim() {
            "xai" => CommandResult::Action(Action::SelectXaiProvider),
            "anthropic" | "gemini" | "openai" | "openrouter" => {
                CommandResult::Action(Action::OpenProviderApiKey(args.trim().to_owned()))
            }
            "" => CommandResult::Error(
                "Usage: /provider <xai|anthropic|gemini|openai|openrouter|custom-name>".to_string(),
            ),
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
    }
}
