use aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamOutput;
use aws_types::request_id::RequestId;

use crate::api_client::ApiClientError;
use crate::api_client::model::ChatResponseStream;

#[derive(Debug)]
struct ToolUseState {
    tool_use_id: String,
    name: String,
}

#[derive(Debug)]
pub struct SendMessageOutputBedrock {
    output: ConverseStreamOutput,
    current_tool: Option<ToolUseState>,
    metadata: Option<aws_sdk_bedrockruntime::types::ConverseStreamMetadataEvent>,
}

impl SendMessageOutputBedrock {
    pub fn new(output: ConverseStreamOutput) -> Self {
        Self {
            output,
            current_tool: None,
            metadata: None,
        }
    }

    pub async fn recv(&mut self) -> Result<Option<ChatResponseStream>, ApiClientError> {
        use aws_sdk_bedrockruntime::types::ConverseStreamOutput as BedrockStream;

        loop {
            tracing::debug!("Receiving from Bedrock stream");
            match self.output.stream.recv().await {
                Ok(Some(event)) => {
                    tracing::debug!("Received Bedrock event: {:?}", event);
                    match event {
                        BedrockStream::ContentBlockStart(start) => {
                            if let Some(start_block) = start.start {
                                if let aws_sdk_bedrockruntime::types::ContentBlockStart::ToolUse(tool_use) = start_block {
                                    tracing::debug!("Tool use start - id: {}, name: {}", tool_use.tool_use_id, tool_use.name);

                                    self.current_tool = Some(ToolUseState {
                                        tool_use_id: tool_use.tool_use_id.clone(),
                                        name: tool_use.name.clone(),
                                    });

                                    return Ok(Some(ChatResponseStream::ToolUseEvent {
                                        tool_use_id: tool_use.tool_use_id,
                                        name: tool_use.name,
                                        input: None,
                                        stop: None,
                                    }));
                                }
                            }
                            continue;
                        }
                        BedrockStream::ContentBlockDelta(delta) => {
                            tracing::debug!("ContentBlockDelta: {:?}", delta);
                            if let Some(content_delta) = delta.delta {
                                match content_delta {
                                    aws_sdk_bedrockruntime::types::ContentBlockDelta::Text(text) => {
                                        tracing::debug!("Text delta: '{}' (len: {})", text, text.len());
                                        if !text.is_empty() {
                                            return Ok(Some(ChatResponseStream::AssistantResponseEvent {
                                                content: text,
                                            }));
                                        }
                                        continue;
                                    }
                                    aws_sdk_bedrockruntime::types::ContentBlockDelta::ToolUse(tool_use) => {
                                        if let Some(ref state) = self.current_tool {
                                            tracing::debug!("Tool use delta - input chunk length: {}", tool_use.input.len());

                                            return Ok(Some(ChatResponseStream::ToolUseEvent {
                                                tool_use_id: state.tool_use_id.clone(),
                                                name: state.name.clone(),
                                                input: Some(tool_use.input),
                                                stop: None,
                                            }));
                                        }
                                        continue;
                                    }
                                    _ => {
                                        tracing::debug!("Other content block delta type (ignoring)");
                                        continue;
                                    }
                                }
                            } else {
                                tracing::debug!("Delta field is None");
                                continue;
                            }
                        }
                        BedrockStream::ContentBlockStop(_) => {
                            if let Some(state) = self.current_tool.take() {
                                tracing::debug!("Tool use stop - id: {}", state.tool_use_id);

                                return Ok(Some(ChatResponseStream::ToolUseEvent {
                                    tool_use_id: state.tool_use_id,
                                    name: state.name,
                                    input: None,
                                    stop: Some(true),
                                }));
                            }
                            continue;
                        }
                        BedrockStream::MessageStop(_) => {
                            tracing::debug!("MessageStop - stream complete");
                            return Ok(None);
                        }
                        BedrockStream::Metadata(metadata) => {
                            tracing::debug!("Metadata event: {:?}", metadata);
                            self.metadata = Some(metadata);
                            continue;
                        }
                        _ => {
                            tracing::debug!("Unknown event type");
                            continue;
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

    pub fn get_metadata(&self) -> Option<&aws_sdk_bedrockruntime::types::ConverseStreamMetadataEvent> {
        self.metadata.as_ref()
    }
}

#[derive(Debug)]
pub enum SendMessageOutput {
    Bedrock(SendMessageOutputBedrock),
    Codewhisperer(
        amzn_codewhisperer_streaming_client::operation::generate_assistant_response::GenerateAssistantResponseOutput,
    ),
    QDeveloper(amzn_qdeveloper_streaming_client::operation::send_message::SendMessageOutput),
    Mock(Vec<ChatResponseStream>),
}

impl SendMessageOutput {
    pub fn request_id(&self) -> Option<&str> {
        match self {
            SendMessageOutput::Bedrock(bedrock) => bedrock.output.request_id(),
            SendMessageOutput::Codewhisperer(output) => output.request_id(),
            SendMessageOutput::QDeveloper(output) => output.request_id(),
            SendMessageOutput::Mock(_) => None,
        }
    }

    pub async fn recv(&mut self) -> Result<Option<ChatResponseStream>, ApiClientError> {
        match self {
            SendMessageOutput::Bedrock(bedrock) => bedrock.recv().await,
            SendMessageOutput::Codewhisperer(output) => Ok(output
                .generate_assistant_response_response
                .recv()
                .await?
                .map(|s| s.into())),
            SendMessageOutput::QDeveloper(output) => Ok(output.send_message_response.recv().await?.map(|s| s.into())),
            SendMessageOutput::Mock(vec) => Ok(vec.pop()),
        }
    }

    pub fn get_bedrock_metadata(&self) -> Option<&aws_sdk_bedrockruntime::types::ConverseStreamMetadataEvent> {
        match self {
            SendMessageOutput::Bedrock(bedrock) => bedrock.get_metadata(),
            _ => None,
        }
    }
}

impl RequestId for SendMessageOutput {
    fn request_id(&self) -> Option<&str> {
        match self {
            SendMessageOutput::Bedrock(bedrock) => bedrock.output.request_id(),
            SendMessageOutput::Codewhisperer(output) => output.request_id(),
            SendMessageOutput::QDeveloper(output) => output.request_id(),
            SendMessageOutput::Mock(_) => Some("<mock-request-id>"),
        }
    }
}
