//! `/provider xai` -- explicitly select the xAI authentication provider.

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
        "/provider xai"
    }

    fn args_required(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        match args.trim() {
            "xai" => CommandResult::Action(Action::SelectXaiProvider),
            "" => CommandResult::Error("Usage: /provider xai".to_string()),
            provider => CommandResult::Error(format!(
                "Unknown provider `{provider}`. Available providers: xai"
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
}
