// Bedrock Converse API integration
use aws_sdk_bedrockruntime::types::{
    ContentBlock,
    ConversationRole,
    Message,
    SystemContentBlock,
    Tool as BedrockTool,
    ToolConfiguration,
    ToolInputSchema,
    ToolSpecification,
};
use aws_smithy_types::Document;
use eyre::Result;

use super::model::{
    ChatMessage,
    Tool,
    UserInputMessage,
    UserInputMessageContext,
};

/// Convert internal message format to Bedrock Message format
pub fn convert_to_bedrock_messages(
    user_input: &UserInputMessage,
    history: Option<&Vec<ChatMessage>>,
) -> Result<Vec<Message>> {
    let mut messages = Vec::new();

    // Add history messages first, skipping empty ones
    if let Some(hist) = history {
        for msg in hist {
            if let Ok(bedrock_msg) = convert_chat_message_to_bedrock(msg) {
                messages.push(bedrock_msg);
            }
        }
    }

    // Add current user message
    let user_message = Message::builder()
        .role(ConversationRole::User)
        .content(ContentBlock::Text(user_input.content.clone()))
        .build()?;
    messages.push(user_message);

    Ok(messages)
}

/// Convert ChatMessage to Bedrock Message
/// Returns Err if the message content is empty/whitespace (should be skipped)
fn convert_chat_message_to_bedrock(msg: &ChatMessage) -> Result<Message> {
    match msg {
        ChatMessage::UserInputMessage(user_msg) => {
            // Skip if content is empty or whitespace
            if user_msg.content.trim().is_empty() {
                eyre::bail!("Empty user message");
            }
            
            let builder = Message::builder()
                .role(ConversationRole::User)
                .content(ContentBlock::Text(user_msg.content.clone()));
            
            Ok(builder.build()?)
        }
        ChatMessage::AssistantResponseMessage(assistant_msg) => {
            // Skip if content is empty or whitespace
            if assistant_msg.content.trim().is_empty() {
                eyre::bail!("Empty assistant message");
            }
            
            let builder = Message::builder()
                .role(ConversationRole::Assistant)
                .content(ContentBlock::Text(assistant_msg.content.clone()));
            
            Ok(builder.build()?)
        }
    }
}

/// Convert internal tools to Bedrock tool configuration
pub fn convert_tools_to_bedrock(tools: Option<&Vec<Tool>>) -> Option<ToolConfiguration> {
    tools.map(|tool_list| {
        let bedrock_tools: Vec<BedrockTool> = tool_list
            .iter()
            .filter_map(|tool| {
                // Extract ToolSpecification from Tool enum
                match tool {
                    Tool::ToolSpecification(spec) => {
                        // Convert FigDocument to aws_smithy_types::Document
                        let document = spec.input_schema.json.as_ref()
                            .map(|fig_doc| Document::from(fig_doc.clone()))?;
                        
                        // Convert tool to Bedrock format
                        let bedrock_spec = ToolSpecification::builder()
                            .name(&spec.name)
                            .description(&spec.description)
                            .input_schema(ToolInputSchema::Json(document))
                            .build()
                            .ok()?;
                        
                        Some(BedrockTool::ToolSpec(bedrock_spec))
                    }
                }
            })
            .collect();

        ToolConfiguration::builder()
            .set_tools(Some(bedrock_tools))
            .build()
            .ok()
    })
    .flatten()
}

/// Extract system prompt from context
pub fn extract_system_prompt(_context: Option<&UserInputMessageContext>) -> Option<Vec<SystemContentBlock>> {
    // For now, return None - can be enhanced later if needed
    None
}
