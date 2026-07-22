//! Legacy `/login` compatibility shim.

use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct LoginCommand;

impl SlashCommand for LoginCommand {
    fn name(&self) -> &str {
        "login"
    }

    fn description(&self) -> &str {
        "Compatibility command: use /provider xai"
    }

    fn usage(&self) -> &str {
        "/login"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Message(
            "`/login` is a compatibility command. Use `/provider xai` to configure xAI authentication."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tc7_login_guides_to_provider_without_starting_authentication() {
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
            LoginCommand.run(&mut ctx, ""),
            CommandResult::Message(message) if message.contains("/provider xai")
        ));
    }
}
