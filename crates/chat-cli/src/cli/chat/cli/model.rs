use amzn_codewhisperer_client::types::Model;
use clap::Args;
use crossterm::style::{
    self,
};
use crossterm::{
    execute,
    queue,
};
use dialoguer::Select;
use serde::{
    Deserialize,
    Serialize,
};

use crate::api_client::Endpoint;
use crate::cli::chat::{
    ChatError,
    ChatSession,
    ChatState,
};
use crate::os::Os;
use crate::theme::StyledText;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Display name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    /// Description of the model
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Actual model id to send in the API
    pub model_id: String,
    /// Size of the model's context window, in tokens
    #[serde(default = "default_context_window")]
    pub context_window_tokens: usize,
    /// Whether the model supports tool use
    #[serde(default)]
    pub supports_tools: bool,
}

impl ModelInfo {
    pub fn from_api_model(model: &Model) -> Self {
        let context_window_tokens = model
            .token_limits()
            .and_then(|limits| limits.max_input_tokens())
            .map_or(default_context_window(), |tokens| tokens as usize);
        Self {
            model_id: model.model_id().to_string(),
            description: model.description.clone(),
            model_name: model.model_name().map(|s| s.to_string()),
            context_window_tokens,
            supports_tools: false,
        }
    }

    /// create a default model with only valid model_id（be compatoble with old stored model data）
    pub fn from_id(model_id: String) -> Self {
        Self {
            model_id,
            description: None,
            model_name: None,
            context_window_tokens: 200_000,
            supports_tools: false,
        }
    }

    pub fn display_name(&self) -> &str {
        self.model_name.as_deref().unwrap_or(&self.model_id)
    }

    pub fn description(&self) -> Option<&str> {
        self.description
            .as_deref()
            .and_then(|d| if d.is_empty() { None } else { Some(d) })
    }
}

/// Command-line arguments for model selection operations
#[deny(missing_docs)]
#[derive(Debug, PartialEq, Args)]
pub struct ModelArgs;
impl ModelArgs {
    pub async fn execute(self, os: &Os, session: &mut ChatSession) -> Result<ChatState, ChatError> {
        Ok(select_model(os, session).await?.unwrap_or(ChatState::PromptUser {
            skip_printing_tools: false,
        }))
    }
}

pub async fn select_model(os: &Os, session: &mut ChatSession) -> Result<Option<ChatState>, ChatError> {
    queue!(session.stderr, style::Print("\n"))?;

    // Fetch available models from service
    let (models, _default_model) = get_available_models(os).await?;

    if models.is_empty() {
        queue!(
            session.stderr,
            StyledText::error_fg(),
            style::Print("No models available\n"),
            StyledText::reset(),
        )?;
        return Ok(None);
    }

    let active_model_id = session.conversation.model_info.as_ref().map(|m| m.model_id.as_str());

    let labels: Vec<String> = models
        .iter()
        .map(|model| {
            let display_name = model.display_name();
            let description = model.description();
            if Some(model.model_id.as_str()) == active_model_id {
                if let Some(desc) = description {
                    format!("{} (active) | {}", display_name, desc)
                } else {
                    format!("{} (active)", display_name)
                }
            } else if let Some(desc) = description {
                format!("{} | {}", display_name, desc)
            } else {
                display_name.to_string()
            }
        })
        .collect();

    let selection: Option<_> = match Select::with_theme(&crate::util::dialoguer_theme())
        .with_prompt("Select a model for this chat session")
        .items(&labels)
        .default(0)
        .interact_on_opt(&dialoguer::console::Term::stdout())
    {
        Ok(sel) => {
            let _ = crossterm::execute!(std::io::stdout(), StyledText::emphasis_fg());
            sel
        },
        // Ctrl‑C -> Err(Interrupted)
        Err(dialoguer::Error::IO(ref e)) if e.kind() == std::io::ErrorKind::Interrupted => return Ok(None),
        Err(e) => return Err(ChatError::Custom(format!("Failed to choose model: {e}").into())),
    };

    queue!(session.stderr, StyledText::reset())?;

    if let Some(index) = selection {
        let selected = models[index].clone();
        session.conversation.model_info = Some(selected.clone());
        let display_name = selected.display_name();

        queue!(
            session.stderr,
            style::Print("\n"),
            style::Print(format!(" Using {}\n\n", display_name)),
            StyledText::reset(),
            StyledText::reset(),
            StyledText::reset(),
        )?;
    }

    execute!(session.stderr, StyledText::reset())?;

    Ok(Some(ChatState::PromptUser {
        skip_printing_tools: false,
    }))
}

pub async fn get_model_info(model_id: &str, os: &Os) -> Result<ModelInfo, ChatError> {
    let (models, _) = get_available_models(os).await?;

    models
        .into_iter()
        .find(|m| m.model_id == model_id)
        .ok_or_else(|| ChatError::Custom(format!("Model '{}' not found", model_id).into()))
}

/// Get available models with caching support
pub async fn get_available_models(os: &Os) -> Result<(Vec<ModelInfo>, ModelInfo), ChatError> {
    let models = get_builtin_models();
    let default_model = get_default_model();
    Ok((models, default_model))
}

/// Returns the context window length in tokens for the given model_id.
/// Uses cached model data when available
pub fn context_window_tokens(model_info: Option<&ModelInfo>) -> usize {
    model_info.map_or_else(default_context_window, |m| m.context_window_tokens)
}

fn default_context_window() -> usize {
    128_000
}

/// Returns the hardcoded list of allowed Bedrock models
fn get_builtin_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            model_id: "openai.gpt-oss-120b-1:0".to_string(),
            model_name: Some("ChatGPT 120B".to_string()),
            description: Some("OpenAI GPT 120B model".to_string()),
            context_window_tokens: 128_000,
            supports_tools: true,
        },
        ModelInfo {
            model_id: "openai.gpt-oss-20b-1:0".to_string(),
            model_name: Some("ChatGPT 20B".to_string()),
            description: Some("OpenAI GPT 20B model".to_string()),
            context_window_tokens: 128_000,
            supports_tools: true,
        },
        ModelInfo {
            model_id: "us.anthropic.claude-haiku-4-5-20251001-v1:0".to_string(),
            model_name: Some("Claude Haiku 4.5".to_string()),
            description: Some("Anthropic Claude Haiku 4.5".to_string()),
            context_window_tokens: 200_000,
            supports_tools: true,
        },
        ModelInfo {
            model_id: "qwen.qwen3-coder-480b-a35b-v1:0".to_string(),
            model_name: Some("Qwen3 Coder 480B".to_string()),
            description: Some("Qwen3 Coder 480B model".to_string()),
            context_window_tokens: 130_000,
            supports_tools: false,
        },
        ModelInfo {
            model_id: "meta.llama4-maverick-17b-instruct-v1:0".to_string(),
            model_name: Some("Llama 4 Maverick 17B".to_string()),
            description: Some("Meta Llama 4 Maverick 17B".to_string()),
            context_window_tokens: 1_000_000,
            supports_tools: false,
        },
        ModelInfo {
            model_id: "deepseek.v3-v1:0".to_string(),
            model_name: Some("DeepSeek V3".to_string()),
            description: Some("DeepSeek V3 model".to_string()),
            context_window_tokens: 163_000,
            supports_tools: false,
        },
    ]
}

/// Returns the default model (ChatGPT 120B)
pub fn get_default_model() -> ModelInfo {
    get_builtin_models()[0].clone()
}

/// Validates that a model ID is in the allowlist
pub fn validate_model_id(model_id: &str) -> eyre::Result<()> {
    if get_builtin_models().iter().any(|m| m.model_id == model_id) {
        Ok(())
    } else {
        eyre::bail!("This model is not supported in this deployment.")
    }
}

/// Checks if a model supports tool use
pub fn model_supports_tools(model_id: &str) -> bool {
    get_builtin_models()
        .iter()
        .find(|m| m.model_id == model_id)
        .map(|m| m.supports_tools)
        .unwrap_or(false)
}

pub fn normalize_model_name(name: &str) -> &str {
    match name {
        "claude-4-sonnet" => "claude-sonnet-4",
        // can add more mapping for backward compatibility
        _ => name,
    }
}

pub fn find_model<'a>(models: &'a [ModelInfo], name: &str) -> Option<&'a ModelInfo> {
    let normalized = normalize_model_name(name);
    models.iter().find(|m| {
        m.model_name
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(normalized))
            || m.model_id.eq_ignore_ascii_case(normalized)
    })
}
