use clap::Args;
use crossterm::style;
use crossterm::queue;
use dialoguer::Select;

use crate::cli::chat::{
    ChatError,
    ChatSession,
    ChatState,
};
use crate::theme::StyledText;

/// Command-line arguments for service tier selection
#[derive(Debug, PartialEq, Args)]
pub struct TierArgs;

impl TierArgs {
    pub async fn execute(self, session: &mut ChatSession) -> Result<ChatState, ChatError> {
        Ok(select_tier(session).await?.unwrap_or(ChatState::PromptUser {
            skip_printing_tools: false,
        }))
    }
}

async fn select_tier(session: &mut ChatSession) -> Result<Option<ChatState>, ChatError> {
    queue!(session.stderr, style::Print("\n"))?;

    let tiers = vec!["flex", "default"];
    let current_tier = &session.conversation.service_tier;

    let labels: Vec<String> = tiers
        .iter()
        .map(|tier| {
            if tier == current_tier {
                format!("{} (active)", tier)
            } else {
                tier.to_string()
            }
        })
        .collect();

    let selection = Select::with_theme(&crate::util::dialoguer_theme())
        .with_prompt("Select service tier")
        .items(&labels)
        .default(tiers.iter().position(|t| t == current_tier).unwrap_or(0))
        .interact_opt()
        .map_err(|_| ChatError::Custom("Selection cancelled".into()))?;

    let Some(index) = selection else {
        return Ok(None);
    };

    let selected_tier = tiers[index];
    session.conversation.service_tier = selected_tier.to_string();

    queue!(
        session.stderr,
        StyledText::emphasis_fg(),
        style::Print(format!("✓ Using service tier: {}\n\n", selected_tier)),
        StyledText::reset(),
    )?;

    Ok(Some(ChatState::PromptUser {
        skip_printing_tools: false,
    }))
}
