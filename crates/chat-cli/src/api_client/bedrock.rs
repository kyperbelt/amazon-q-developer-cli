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
    model_system_prompt: Option<&str>,
) -> Result<Vec<Message>> {
    let mut messages = Vec::new();

    // Add history messages first, skipping empty ones
    if let Some(hist) = history {
        for msg in hist {
            if let Ok(bedrock_msg) = convert_chat_message_to_bedrock(msg) {
                tracing::debug!("Adding history message with {} content blocks", bedrock_msg.content().len());
                messages.push(bedrock_msg);
            }
        }
    }

    // Add current user message
    let mut content_blocks = Vec::new();
    
    // Only add text if not empty
    if !user_input.content.trim().is_empty() {
        content_blocks.push(ContentBlock::Text(user_input.content.clone()));
    }
    
    // Add tool results if present
    if let Some(context) = &user_input.user_input_message_context {
        if let Some(tool_results) = &context.tool_results {
            for result in tool_results {
                let status = match result.status {
                    crate::api_client::model::ToolResultStatus::Success => 
                        aws_sdk_bedrockruntime::types::ToolResultStatus::Success,
                    crate::api_client::model::ToolResultStatus::Error => 
                        aws_sdk_bedrockruntime::types::ToolResultStatus::Error,
                };
                
                let mut result_content = Vec::new();
                for item in &result.content {
                    match item {
                        crate::api_client::model::ToolResultContentBlock::Text(text) => {
                            result_content.push(
                                aws_sdk_bedrockruntime::types::ToolResultContentBlock::Text(text.clone())
                            );
                        }
                        crate::api_client::model::ToolResultContentBlock::Json(doc) => {
                            result_content.push(
                                aws_sdk_bedrockruntime::types::ToolResultContentBlock::Json(doc.clone())
                            );
                        }
                    }
                }
                
                content_blocks.push(
                    ContentBlock::ToolResult(
                        aws_sdk_bedrockruntime::types::ToolResultBlock::builder()
                            .tool_use_id(&result.tool_use_id)
                            .set_content(Some(result_content))
                            .status(status)
                            .build()?
                    )
                );
            }
        }
    }
    
    // Only add user message if we have content
    if !content_blocks.is_empty() {
        let mut builder = Message::builder().role(ConversationRole::User);
        for block in content_blocks {
            builder = builder.content(block);
        }
        let user_message = builder.build()?;
        tracing::debug!("Adding current user message with {} content blocks", user_message.content().len());
        messages.push(user_message);
    }

    // Debug log all messages
    for (i, msg) in messages.iter().enumerate() {
        tracing::debug!("Message {}: role={:?}, content_blocks={}", i, msg.role(), msg.content().len());
        for (j, block) in msg.content().iter().enumerate() {
            match block {
                ContentBlock::Text(text) => {
                    tracing::debug!("  Block {}: Text (len={})", j, text.len());
                    if text.trim().is_empty() {
                        tracing::warn!("  WARNING: Empty text block detected!");
                    }
                }
                ContentBlock::ToolUse(tool_use) => {
                    tracing::debug!("  Block {}: ToolUse (id={}, name={})", j, tool_use.tool_use_id, tool_use.name);
                }
                ContentBlock::ToolResult(tool_result) => {
                    tracing::debug!("  Block {}: ToolResult (id={}, status={:?})", j, tool_result.tool_use_id, tool_result.status);
                }
                _ => {
                    tracing::debug!("  Block {}: Other", j);
                }
            }
        }
    }

    Ok(messages)
}

/// Convert ChatMessage to Bedrock Message
/// Returns Err if the message content is empty/whitespace (should be skipped)
fn convert_chat_message_to_bedrock(msg: &ChatMessage) -> Result<Message> {
    match msg {
        ChatMessage::UserInputMessage(user_msg) => {
            let mut content_blocks = Vec::new();
            
            // Add text content if not empty
            if !user_msg.content.trim().is_empty() {
                content_blocks.push(ContentBlock::Text(user_msg.content.clone()));
            }
            
            // Add tool results if present
            if let Some(context) = &user_msg.user_input_message_context {
                if let Some(tool_results) = &context.tool_results {
                    for result in tool_results {
                        let status = match result.status {
                            crate::api_client::model::ToolResultStatus::Success => 
                                aws_sdk_bedrockruntime::types::ToolResultStatus::Success,
                            crate::api_client::model::ToolResultStatus::Error => 
                                aws_sdk_bedrockruntime::types::ToolResultStatus::Error,
                        };
                        
                        let mut result_content = Vec::new();
                        for item in &result.content {
                            match item {
                                crate::api_client::model::ToolResultContentBlock::Text(text) => {
                                    result_content.push(
                                        aws_sdk_bedrockruntime::types::ToolResultContentBlock::Text(text.clone())
                                    );
                                }
                                crate::api_client::model::ToolResultContentBlock::Json(doc) => {
                                    result_content.push(
                                        aws_sdk_bedrockruntime::types::ToolResultContentBlock::Json(doc.clone())
                                    );
                                }
                            }
                        }
                        
                        content_blocks.push(
                            ContentBlock::ToolResult(
                                aws_sdk_bedrockruntime::types::ToolResultBlock::builder()
                                    .tool_use_id(&result.tool_use_id)
                                    .set_content(Some(result_content))
                                    .status(status)
                                    .build()?
                            )
                        );
                    }
                }
            }
            
            // Skip if no content blocks
            if content_blocks.is_empty() {
                eyre::bail!("Empty user message");
            }
            
            let mut builder = Message::builder().role(ConversationRole::User);
            for block in content_blocks {
                builder = builder.content(block);
            }
            
            Ok(builder.build()?)
        }
        ChatMessage::AssistantResponseMessage(assistant_msg) => {
            let mut content_blocks = Vec::new();
            
            // Add text content if not empty
            if !assistant_msg.content.trim().is_empty() {
                content_blocks.push(ContentBlock::Text(assistant_msg.content.clone()));
            }
            
            // Add tool uses if present
            if let Some(tool_uses) = &assistant_msg.tool_uses {
                for tool_use in tool_uses {
                    content_blocks.push(
                        ContentBlock::ToolUse(
                            aws_sdk_bedrockruntime::types::ToolUseBlock::builder()
                                .tool_use_id(&tool_use.tool_use_id)
                                .name(&tool_use.name)
                                .input(Document::from(tool_use.input.clone()))
                                .build()?
                        )
                    );
                }
            }
            
            // Skip if no content blocks
            if content_blocks.is_empty() {
                eyre::bail!("Empty assistant message");
            }
            
            let mut builder = Message::builder().role(ConversationRole::Assistant);
            for block in content_blocks {
                builder = builder.content(block);
            }
            
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

/// Extract system prompt from model and agent configuration
pub fn extract_system_prompt(
    model_system_prompt: Option<&str>,
    agent_prompt: Option<&str>,
) -> Option<Vec<SystemContentBlock>> {
    let mut blocks = Vec::new();
    
    // Model system prompt first
    if let Some(prompt) = model_system_prompt {
        tracing::debug!("Adding model system prompt: {}", prompt);
        blocks.push(SystemContentBlock::Text(prompt.to_string()));
    } else {
        tracing::debug!("No model system prompt");
    }
    
    // Agent prompt second
    if let Some(prompt) = agent_prompt {
        tracing::debug!("Adding agent prompt: {}", prompt);
        blocks.push(SystemContentBlock::Text(prompt.to_string()));
    } else {
        tracing::debug!("No agent prompt");
    }
    
    tracing::debug!("Total system prompt blocks: {}", blocks.len());
    
    if blocks.is_empty() {
        None
    } else {
        Some(blocks)
    }
}
