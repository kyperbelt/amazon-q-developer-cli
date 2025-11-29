use aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamOutput;
use aws_types::request_id::RequestId;

use crate::api_client::ApiClientError;
use crate::api_client::model::ChatResponseStream;

#[derive(Debug)]
pub enum SendMessageOutput {
    Bedrock(ConverseStreamOutput),
    Codewhisperer(
        amzn_codewhisperer_streaming_client::operation::generate_assistant_response::GenerateAssistantResponseOutput,
    ),
    QDeveloper(amzn_qdeveloper_streaming_client::operation::send_message::SendMessageOutput),
    Mock(Vec<ChatResponseStream>),
}

impl SendMessageOutput {
    pub fn request_id(&self) -> Option<&str> {
        match self {
            SendMessageOutput::Bedrock(output) => output.request_id(),
            SendMessageOutput::Codewhisperer(output) => output.request_id(),
            SendMessageOutput::QDeveloper(output) => output.request_id(),
            SendMessageOutput::Mock(_) => None,
        }
    }

    pub async fn recv(&mut self) -> Result<Option<ChatResponseStream>, ApiClientError> {
        match self {
            SendMessageOutput::Bedrock(output) => {
                use aws_sdk_bedrockruntime::types::ConverseStreamOutput as BedrockStream;
                
                loop {
                    tracing::debug!("Receiving from Bedrock stream");
                    match output.stream.recv().await {
                        Ok(Some(event)) => {
                            tracing::debug!("Received Bedrock event: {:?}", event);
                            match event {
                                BedrockStream::ContentBlockStart(_) => {
                                    tracing::debug!("ContentBlockStart - continuing");
                                    continue; // Keep reading
                                }
                                BedrockStream::ContentBlockDelta(delta) => {
                                    tracing::debug!("ContentBlockDelta: {:?}", delta);
                                    if let Some(content_delta) = delta.delta {
                                        match content_delta {
                                            aws_sdk_bedrockruntime::types::ContentBlockDelta::Text(text) => {
                                                tracing::debug!("Text delta: '{}' (len: {})", text, text.len());
                                                // Only return non-empty text
                                                if !text.is_empty() {
                                                    return Ok(Some(ChatResponseStream::AssistantResponseEvent {
                                                        content: text,
                                                    }));
                                                }
                                                // Empty text - keep reading
                                                continue;
                                            }
                                            aws_sdk_bedrockruntime::types::ContentBlockDelta::ToolUse(tool_use) => {
                                                tracing::debug!("Tool use delta - input: {:?}", tool_use.input);
                                                // TODO: Properly handle tool use events
                                                // The parser expects specific event ordering that needs more work
                                                continue; // Keep reading for now
                                            }
                                            _ => {
                                                tracing::debug!("Other content block delta type (ignoring)");
                                                continue; // Keep reading
                                            }
                                        }
                                    } else {
                                        tracing::debug!("Delta field is None");
                                        continue; // Keep reading
                                    }
                                }
                                BedrockStream::ContentBlockStop(_) => {
                                    tracing::debug!("ContentBlockStop - continuing");
                                    continue; // Keep reading
                                }
                                BedrockStream::MessageStop(_) => {
                                    tracing::debug!("MessageStop - stream complete");
                                    return Ok(None); // End of stream
                                }
                                BedrockStream::Metadata(metadata) => {
                                    tracing::debug!("Metadata event: {:?}", metadata);
                                    continue; // Keep reading
                                }
                                _ => {
                                    tracing::debug!("Unknown event type");
                                    continue; // Keep reading
                                }
                            }
                        }
                        Ok(None) => {
                            tracing::debug!("Stream ended (None)");
                            return Ok(None);
                        }
                        Err(e) => {
                            tracing::error!("Stream error: {:?}", e);
                            return Err(ApiClientError::SmithyBuild(
                                aws_smithy_types::error::operation::BuildError::other(e)
                            ));
                        }
                    }
                }
            }
            SendMessageOutput::Codewhisperer(output) => Ok(output
                .generate_assistant_response_response
                .recv()
                .await?
                .map(|s| s.into())),
            SendMessageOutput::QDeveloper(output) => Ok(output.send_message_response.recv().await?.map(|s| s.into())),
            SendMessageOutput::Mock(vec) => Ok(vec.pop()),
        }
    }
}

impl RequestId for SendMessageOutput {
    fn request_id(&self) -> Option<&str> {
        match self {
            SendMessageOutput::Bedrock(output) => output.request_id(),
            SendMessageOutput::Codewhisperer(output) => output.request_id(),
            SendMessageOutput::QDeveloper(output) => output.request_id(),
            SendMessageOutput::Mock(_) => Some("<mock-request-id>"),
        }
    }
}
