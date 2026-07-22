//! Legacy `/logout` compatibility shim.

use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct LogoutCommand;

impl SlashCommand for LogoutCommand {
    fn name(&self) -> &str {
        "logout"
    }

    fn description(&self) -> &str {
        "Compatibility command: manage xAI with /provider xai"
    }

    fn usage(&self) -> &str {
        "/logout"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Message(
            "`/logout` is a compatibility command. Manage xAI authentication with `/provider xai`."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tc8_logout_guides_to_provider_without_clearing_global_auth() {
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
            LogoutCommand.run(&mut ctx, ""),
            CommandResult::Message(message) if message.contains("/provider xai")
        ));
    }
}
