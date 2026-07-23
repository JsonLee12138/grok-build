//! `/provider <xai|anthropic|gemini|openai|openrouter>` -- configure authentication.

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
        "/provider <xai|anthropic|gemini|openai|openrouter>"
    }

    fn args_required(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        match args.trim() {
            "xai" => CommandResult::Action(Action::SelectXaiProvider),
            "anthropic" => CommandResult::Action(Action::OpenProviderApiKey(
                xai_grok_shell::auth::provider_registry::ProviderId::Anthropic,
            )),
            "gemini" => CommandResult::Action(Action::OpenProviderApiKey(
                xai_grok_shell::auth::provider_registry::ProviderId::Gemini,
            )),
            "openai" => CommandResult::Action(Action::OpenProviderApiKey(
                xai_grok_shell::auth::provider_registry::ProviderId::Openai,
            )),
            "openrouter" => CommandResult::Action(Action::OpenProviderApiKey(
                xai_grok_shell::auth::provider_registry::ProviderId::Openrouter,
            )),
            "" => CommandResult::Error(
                "Usage: /provider <xai|anthropic|gemini|openai|openrouter>".to_string(),
            ),
            provider => CommandResult::Error(format!(
                "Unknown provider `{provider}`. Available providers: xai, anthropic, gemini, openai, openrouter"
            )),
        }
    }
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
            CommandResult::Action(Action::OpenProviderApiKey(
                xai_grok_shell::auth::provider_registry::ProviderId::Gemini
            ))
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
            command.run(&mut ctx, "other"),
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
            CommandResult::Action(Action::OpenProviderApiKey(
                xai_grok_shell::auth::provider_registry::ProviderId::Anthropic
            ))
        ));
        assert!(matches!(
            command.run(&mut ctx, "openai"),
            CommandResult::Action(Action::OpenProviderApiKey(
                xai_grok_shell::auth::provider_registry::ProviderId::Openai
            ))
        ));
        assert!(matches!(
            command.run(&mut ctx, "openrouter"),
            CommandResult::Action(Action::OpenProviderApiKey(
                xai_grok_shell::auth::provider_registry::ProviderId::Openrouter
            ))
        ));
    }
}
