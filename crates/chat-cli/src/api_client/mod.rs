mod bedrock;
mod credentials;
pub mod customization;
mod delay_interceptor;
mod endpoints;
pub mod error;
pub mod model;
mod opt_out;
pub mod profile;
mod retry_classifier;
pub mod send_message_output;
use std::sync::Arc;
use std::time::Duration;

use amzn_codewhisperer_client::Client as CodewhispererClient;
use amzn_codewhisperer_client::operation::create_subscription_token::CreateSubscriptionTokenOutput;
use amzn_codewhisperer_client::types::Origin::Cli;
use amzn_codewhisperer_client::types::{
    Model,
    OptInFeatureToggle,
    OptOutPreference,
    SubscriptionStatus,
    TelemetryEvent,
    UserContext,
};
use amzn_codewhisperer_streaming_client::Client as CodewhispererStreamingClient;
use amzn_qdeveloper_streaming_client::Client as QDeveloperStreamingClient;
use amzn_qdeveloper_streaming_client::types::Origin;
use aws_config::retry::RetryConfig;
use aws_config::timeout::TimeoutConfig;
use aws_credential_types::Credentials;
use aws_credential_types::provider::ProvideCredentials;
use aws_sdk_ssooidc::error::ProvideErrorMetadata;
use aws_types::request_id::RequestId;
use aws_types::sdk_config::StalledStreamProtectionConfig;
pub use endpoints::Endpoint;
pub use error::ApiClientError;
use error::{
    ConverseStreamError,
    ConverseStreamErrorKind,
};
use parking_lot::Mutex;
pub use profile::list_available_profiles;
use serde_json::Map;
use tokio::sync::RwLock;
use tracing::{
    debug,
    error,
};

use crate::api_client::credentials::CredentialsChain;
use crate::api_client::delay_interceptor::DelayTrackingInterceptor;
use crate::api_client::model::{
    ChatResponseStream,
    ConversationState,
};
use crate::api_client::opt_out::OptOutInterceptor;
use crate::api_client::send_message_output::SendMessageOutput;
use crate::auth::builder_id::BearerResolver;
use crate::aws_common::{
    UserAgentOverrideInterceptor,
    app_name,
    behavior_version,
};
use crate::database::settings::Setting;
use crate::database::{
    AuthProfile,
    Database,
};
use crate::os::{
    Env,
    Fs,
};
use crate::util::env_var::is_integ_test;

// Opt out constants
pub const X_AMZN_CODEWHISPERER_OPT_OUT_HEADER: &str = "x-amzn-codewhisperer-optout";

// TODO(bskiser): confirm timeout is updated to an appropriate value?
const DEFAULT_TIMEOUT_DURATION: Duration = Duration::from_secs(60 * 5);

pub const MAX_RETRY_DELAY_DURATION: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct ModelListResult {
    pub models: Vec<Model>,
    pub default_model: Model,
}

impl From<ModelListResult> for (Vec<Model>, Model) {
    fn from(v: ModelListResult) -> Self {
        (v.models, v.default_model)
    }
}

#[derive(Clone, Debug)]
pub struct ApiClient {
    bedrock_client: aws_sdk_bedrockruntime::Client,
    // Keep legacy client for telemetry and other non-chat operations
    client: CodewhispererClient,
    mock_client: Option<Arc<Mutex<std::vec::IntoIter<Vec<ChatResponseStream>>>>>,
    profile: Option<AuthProfile>,
}

impl ApiClient {
    pub async fn new(
        env: &Env,
        fs: &Fs,
        database: &mut Database,
        endpoint: Option<Endpoint>,
    ) -> Result<Self, ApiClientError> {
        // Load AWS config for Bedrock
        let aws_config = aws_config::load_from_env().await;
        let bedrock_client = aws_sdk_bedrockruntime::Client::new(&aws_config);

        // Keep legacy client for telemetry (uses dummy credentials)
        let endpoint = endpoint.unwrap_or(Endpoint::configured_value(database));
        let credentials = Credentials::new("xxx", "xxx", None, None, "xxx");
        let bearer_sdk_config = aws_config::defaults(behavior_version())
            .region(endpoint.region.clone())
            .credentials_provider(credentials)
            .timeout_config(timeout_config(database))
            .retry_config(retry_config())
            .load()
            .await;

        let client = CodewhispererClient::from_conf(
            amzn_codewhisperer_client::config::Builder::from(&bearer_sdk_config)
                .http_client(crate::aws_common::http_client::client())
                .interceptor(OptOutInterceptor::new(database))
                .interceptor(UserAgentOverrideInterceptor::new())
                .app_name(app_name())
                .endpoint_url(endpoint.url())
                .build(),
        );

        // Handle test mocking
        if cfg!(test) && !is_integ_test() {
            let mut this = Self {
                bedrock_client,
                client,
                mock_client: None,
                profile: None,
            };

            if let Some(json) = crate::util::env_var::get_mock_chat_response(env) {
                this.set_mock_output(serde_json::from_str(fs.read_to_string(json).await.unwrap().as_str()).unwrap());
            }

            return Ok(this);
        }

        Ok(Self {
            bedrock_client,
            client,
            mock_client: None,
            profile: None,
        })
    }

    pub async fn send_telemetry_event(
        &self,
        telemetry_event: TelemetryEvent,
        user_context: UserContext,
        telemetry_enabled: bool,
        model: Option<String>,
    ) -> Result<(), ApiClientError> {
        if cfg!(test) {
            return Ok(());
        }

        self.client
            .send_telemetry_event()
            .telemetry_event(telemetry_event)
            .user_context(user_context)
            .opt_out_preference(match telemetry_enabled {
                true => OptOutPreference::OptIn,
                false => OptOutPreference::OptOut,
            })
            .set_profile_arn(self.profile.as_ref().map(|p| p.arn.clone()))
            .set_model_id(model)
            .send()
            .await?;

        Ok(())
    }

    pub async fn list_available_profiles(&self) -> Result<Vec<AuthProfile>, ApiClientError> {
        if cfg!(test) {
            return Ok(vec![
                AuthProfile {
                    arn: "my:arn:1".to_owned(),
                    profile_name: "MyProfile".to_owned(),
                },
                AuthProfile {
                    arn: "my:arn:2".to_owned(),
                    profile_name: "MyOtherProfile".to_owned(),
                },
            ]);
        }

        let mut profiles = vec![];
        let mut stream = self.client.list_available_profiles().into_paginator().send();
        while let Some(profiles_output) = stream.next().await {
            profiles.extend(profiles_output?.profiles().iter().cloned().map(AuthProfile::from));
        }

        Ok(profiles)
    }

    // Legacy function - no longer used with Bedrock
    pub async fn list_available_models(&self) -> Result<ModelListResult, ApiClientError> {
        if cfg!(test) {
            let m = Model::builder()
                .model_id("model-1")
                .description("Test Model 1")
                .build()
                .unwrap();

            return Ok(ModelListResult {
                models: vec![m.clone()],
                default_model: m,
            });
        }
        Err(ApiClientError::DefaultModelNotFound)
    }

    // Legacy function - no longer used with Bedrock
    pub async fn list_available_models_cached(&self) -> Result<ModelListResult, ApiClientError> {
        self.list_available_models().await
    }

    // Legacy function - no longer used with Bedrock
    pub async fn invalidate_model_cache(&self) {
        // No-op
    }

    // Legacy function - no longer used with Bedrock
    pub async fn get_available_models(&self, _region: &str) -> Result<ModelListResult, ApiClientError> {
        self.list_available_models().await
    }

    pub async fn is_mcp_enabled(&self) -> Result<bool, ApiClientError> {
        // MCP is always enabled in Bedrock mode
        Ok(true)
    }

    pub async fn create_subscription_token(&self) -> Result<CreateSubscriptionTokenOutput, ApiClientError> {
        if cfg!(test) {
            return Ok(CreateSubscriptionTokenOutput::builder()
                .set_encoded_verification_url(Some("test/url".to_string()))
                .set_status(Some(SubscriptionStatus::Inactive))
                .set_token(Some("test-token".to_string()))
                .build()?);
        }

        self.client
            .create_subscription_token()
            .send()
            .await
            .map_err(ApiClientError::CreateSubscriptionToken)
    }

    pub async fn send_message(
        &self,
        conversation: ConversationState,
    ) -> Result<SendMessageOutput, ConverseStreamError> {
        debug!("Sending conversation: {:#?}", conversation);

        let ConversationState {
            conversation_id,
            user_input_message,
            history,
            service_tier,
            model_system_prompt,
            agent_prompt,
        } = conversation;

        let model_id = user_input_message.model_id.clone()
            .unwrap_or_else(|| crate::cli::chat::cli::model::get_default_model().model_id);

        debug!("Sending message to Bedrock with model: {}", model_id);

        // Validate model ID
        if let Err(e) = crate::cli::chat::cli::model::validate_model_id(&model_id) {
            return Err(ConverseStreamError::new(
                ConverseStreamErrorKind::InvalidModel,
                None::<aws_sdk_bedrockruntime::Error>,
            ));
        }

        // Handle mock client for testing
        if let Some(client) = &self.mock_client {
            let mut new_events = client.lock().next().unwrap_or_default().clone();
            new_events.reverse();
            return Ok(SendMessageOutput::Mock(new_events));
        }

        // Convert to Bedrock format
        let messages = bedrock::convert_to_bedrock_messages(
            &user_input_message,
            history.as_ref(),
            model_system_prompt.as_deref(),
        )
            .map_err(|e| {
                debug!("Failed to convert messages: {}", e);
                ConverseStreamError::new(
                    ConverseStreamErrorKind::MessageConversion,
                    None::<aws_sdk_bedrockruntime::Error>,
                )
            })?;

        debug!("Converted {} messages for Bedrock", messages.len());

        // Check if model supports tools by looking it up in the builtin list
        let supports_tools = crate::cli::chat::cli::model::model_supports_tools(&model_id);

        let tools = bedrock::convert_tools_to_bedrock(
            user_input_message.user_input_message_context.as_ref()
                .and_then(|ctx| ctx.tools.as_ref())
        );

        let system_prompt = bedrock::extract_system_prompt(
            model_system_prompt.as_deref(),
            agent_prompt.as_deref(),
        );

        // Call Bedrock Converse Stream API
        debug!("Calling Bedrock converse_stream API");
        let mut request = self.bedrock_client
            .converse_stream()
            .model_id(model_id.clone())
            .set_messages(Some(messages));

        // Only pass tools if model supports them
        if supports_tools {
            if let Some(tool_config) = tools {
                debug!("Sending {} tools to Bedrock (model supports tools)", tool_config.tools().len());
                request = request.tool_config(tool_config);
            }
        } else {
            debug!("Model does not support tools, skipping tool config");
        }

        // Set service tier
        if let Some(tier) = service_tier {
            let tier_type = match tier.as_str() {
                "flex" => aws_sdk_bedrockruntime::types::ServiceTierType::Flex,
                _ => aws_sdk_bedrockruntime::types::ServiceTierType::Default,
            };
            let service_tier = aws_sdk_bedrockruntime::types::ServiceTier::builder()
                .r#type(tier_type)
                .build()?;
            request = request.service_tier(service_tier);
            debug!("Using service tier: {}", tier);
        }

        match request.send().await {
            Ok(output) => {
                debug!("Bedrock request successful, returning stream");
                Ok(SendMessageOutput::Bedrock(
                    crate::api_client::send_message_output::SendMessageOutputBedrock::new(output)
                ))
            }
            Err(err) => {
                debug!("Bedrock request failed: {:?}", err);
                let request_id = err.meta().request_id().map(|s| s.to_string());
                let status_code = err.raw_response().map(|res| res.status().as_u16());

                // Check for region-specific errors
                let error_kind = if let Some(code) = status_code {
                    if code == 404 {
                        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "unknown".to_string());
                        tracing::error!("Model {} may not be available in region {}", model_id, region);
                        ConverseStreamErrorKind::ModelNotAvailable
                    } else {
                        ConverseStreamErrorKind::ApiError
                    }
                } else {
                    ConverseStreamErrorKind::ApiError
                };

                Err(ConverseStreamError::new(error_kind, Some(err))
                    .set_request_id(request_id)
                    .set_status_code(status_code))
            }
        }
    }

    /// Only meant for testing. Do not use outside of testing responses.
    pub fn set_mock_output(&mut self, json: serde_json::Value) {
        let mut mock = Vec::new();
        for response in json.as_array().unwrap() {
            let mut stream = Vec::new();
            for event in response.as_array().unwrap() {
                match event {
                    serde_json::Value::String(assistant_text) => {
                        stream.push(ChatResponseStream::AssistantResponseEvent {
                            content: assistant_text.clone(),
                        });
                    },
                    serde_json::Value::Object(tool_use) => {
                        stream.append(&mut split_tool_use_event(tool_use));
                    },
                    other => panic!("Unexpected value: {:?}", other),
                }
            }
            mock.push(stream);
        }

        self.mock_client = Some(Arc::new(Mutex::new(mock.into_iter())));
    }

    // Add a helper method to check if using non-default endpoint
    fn is_custom_endpoint(database: &Database) -> bool {
        database.settings.get(Setting::ApiCodeWhispererService).is_some()
    }
}

fn classify_error_kind<T: ProvideErrorMetadata, R>(
    status_code: Option<u16>,
    body: &[u8],
    model_id_opt: Option<&str>,
    sdk_error: &error::SdkError<T, R>,
) -> ConverseStreamErrorKind {
    let contains = |haystack: &[u8], needle: &[u8]| haystack.windows(needle.len()).any(|v| v == needle);

    let is_throttling = status_code.is_some_and(|status| status == 429);
    let is_context_window_overflow = contains(body, b"Input is too long.");
    let is_model_unavailable = contains(body, b"INSUFFICIENT_MODEL_CAPACITY")
        // Legacy error response fallback
        || (model_id_opt.is_some()
            && status_code.is_some_and(|status| status == 500)
            && contains(
                body,
                b"Encountered unexpectedly high load when processing the request, please try again.",
            ));
    let is_monthly_limit_err = contains(body, b"MONTHLY_REQUEST_COUNT");

    if is_context_window_overflow {
        return ConverseStreamErrorKind::ContextWindowOverflow;
    }

    // Both ModelOverloadedError and Throttling return 429,
    // so check is_model_unavailable first.
    if is_model_unavailable {
        return ConverseStreamErrorKind::ModelOverloadedError;
    }

    if is_throttling {
        return ConverseStreamErrorKind::Throttling;
    }

    if is_monthly_limit_err {
        return ConverseStreamErrorKind::MonthlyLimitReached;
    }

    ConverseStreamErrorKind::Unknown {
        // do not change - we currently use sdk_error_code for mapping from an arbitrary sdk error
        // to a reason code.
        reason_code: error::sdk_error_code(sdk_error),
    }
}

fn timeout_config(database: &Database) -> TimeoutConfig {
    let timeout = database
        .settings
        .get_int(Setting::ApiTimeout)
        .and_then(|i| i.try_into().ok())
        .map_or(DEFAULT_TIMEOUT_DURATION, Duration::from_millis);

    TimeoutConfig::builder()
        .read_timeout(timeout)
        .operation_timeout(timeout)
        .operation_attempt_timeout(timeout)
        .connect_timeout(timeout)
        .build()
}

fn retry_config() -> RetryConfig {
    RetryConfig::adaptive()
        .with_max_attempts(3)
        .with_max_backoff(MAX_RETRY_DELAY_DURATION)
}

pub fn stalled_stream_protection_config() -> StalledStreamProtectionConfig {
    StalledStreamProtectionConfig::enabled()
        .grace_period(Duration::from_secs(60 * 5))
        .build()
}

fn split_tool_use_event(value: &Map<String, serde_json::Value>) -> Vec<ChatResponseStream> {
    let tool_use_id = value.get("tool_use_id").unwrap().as_str().unwrap().to_string();
    let name = value.get("name").unwrap().as_str().unwrap().to_string();
    let args_str = value.get("args").unwrap().to_string();
    let split_point = args_str.len() / 2;
    vec![
        ChatResponseStream::ToolUseEvent {
            tool_use_id: tool_use_id.clone(),
            name: name.clone(),
            input: None,
            stop: None,
        },
        ChatResponseStream::ToolUseEvent {
            tool_use_id: tool_use_id.clone(),
            name: name.clone(),
            input: Some(args_str.split_at(split_point).0.to_string()),
            stop: None,
        },
        ChatResponseStream::ToolUseEvent {
            tool_use_id: tool_use_id.clone(),
            name: name.clone(),
            input: Some(args_str.split_at(split_point).1.to_string()),
            stop: None,
        },
        ChatResponseStream::ToolUseEvent {
            tool_use_id: tool_use_id.clone(),
            name: name.clone(),
            input: None,
            stop: Some(true),
        },
    ]
}

#[cfg(test)]
mod tests {
    use amzn_codewhisperer_client::types::{
        ChatAddMessageEvent,
        IdeCategory,
        OperatingSystem,
    };
    use bstr::ByteSlice;

    use super::*;
    use crate::api_client::model::UserInputMessage;

    #[tokio::test]
    async fn create_clients() {
        let env = Env::new();
        let fs = Fs::new();
        let mut database = crate::database::Database::new().await.unwrap();
        let _ = ApiClient::new(&env, &fs, &mut database, None).await;
    }

    #[tokio::test]
    async fn test_mock() {
        let env = Env::new();
        let fs = Fs::new();
        let mut database = crate::database::Database::new().await.unwrap();
        let mut client = ApiClient::new(&env, &fs, &mut database, None).await.unwrap();
        client
            .send_telemetry_event(
                TelemetryEvent::ChatAddMessageEvent(
                    ChatAddMessageEvent::builder()
                        .conversation_id("<conversation-id>")
                        .message_id("<message-id>")
                        .build()
                        .unwrap(),
                ),
                UserContext::builder()
                    .ide_category(IdeCategory::Cli)
                    .operating_system(OperatingSystem::Linux)
                    .product("<product>")
                    .build()
                    .unwrap(),
                false,
                Some("model".to_owned()),
            )
            .await
            .unwrap();

        client.mock_client = Some(Arc::new(Mutex::new(
            vec![vec![
                ChatResponseStream::AssistantResponseEvent {
                    content: "Hello!".to_owned(),
                },
                ChatResponseStream::AssistantResponseEvent {
                    content: " How can I".to_owned(),
                },
                ChatResponseStream::AssistantResponseEvent {
                    content: " assist you today?".to_owned(),
                },
            ]]
            .into_iter(),
        )));

        let mut output = client
            .send_message(ConversationState {
                conversation_id: None,
                user_input_message: UserInputMessage {
                    images: None,
                    content: "Hello".into(),
                    user_input_message_context: None,
                    user_intent: None,
                    model_id: Some("model".to_owned()),
                },
                history: None,
            })
            .await
            .unwrap();

        let mut output_content = String::new();
        while let Some(ChatResponseStream::AssistantResponseEvent { content }) = output.recv().await.unwrap() {
            output_content.push_str(&content);
        }
        assert_eq!(output_content, "Hello! How can I assist you today?");
    }

    #[test]
    fn test_classify_error_kind() {
        use aws_smithy_runtime_api::http::Response;
        use aws_smithy_types::body::SdkBody;

        use crate::api_client::error::{
            GenerateAssistantResponseError,
            SdkError,
        };

        let mock_sdk_error = || {
            SdkError::service_error(
                GenerateAssistantResponseError::unhandled("test"),
                Response::new(500.try_into().unwrap(), SdkBody::empty()),
            )
        };

        let test_cases: Vec<(Option<u16>, &[u8], Option<&str>, ConverseStreamErrorKind)> = vec![
            (
                Some(400),
                b"Input is too long.",
                None,
                ConverseStreamErrorKind::ContextWindowOverflow,
            ),
            (
                Some(500),
                b"INSUFFICIENT_MODEL_CAPACITY",
                Some("model-1"),
                ConverseStreamErrorKind::ModelOverloadedError,
            ),
            (
                Some(500),
                b"Encountered unexpectedly high load when processing the request, please try again.",
                Some("model-1"),
                ConverseStreamErrorKind::ModelOverloadedError,
            ),
            (
                Some(429),
                b"Rate limit exceeded",
                None,
                ConverseStreamErrorKind::Throttling,
            ),
            (
                Some(400),
                b"MONTHLY_REQUEST_COUNT exceeded",
                None,
                ConverseStreamErrorKind::MonthlyLimitReached,
            ),
            (
                Some(429),
                b"Input is too long.",
                None,
                ConverseStreamErrorKind::ContextWindowOverflow,
            ),
            (
                Some(429),
                b"INSUFFICIENT_MODEL_CAPACITY",
                Some("model-1"),
                ConverseStreamErrorKind::ModelOverloadedError,
            ),
            (
                Some(500),
                b"Encountered unexpectedly high load when processing the request, please try again.",
                None,
                ConverseStreamErrorKind::Unknown {
                    reason_code: "test".to_string(),
                },
            ),
            (
                Some(400),
                b"Encountered unexpectedly high load when processing the request, please try again.",
                Some("model-1"),
                ConverseStreamErrorKind::Unknown {
                    reason_code: "test".to_string(),
                },
            ),
            (Some(500), b"Some other error", None, ConverseStreamErrorKind::Unknown {
                reason_code: "test".to_string(),
            }),
        ];

        for (status_code, body, model_id, expected) in test_cases {
            let result = classify_error_kind(status_code, body, model_id, &mock_sdk_error());
            assert_eq!(
                std::mem::discriminant(&result),
                std::mem::discriminant(&expected),
                "expected '{}', got '{}' | status_code: {:?}, body: '{}', model_id: '{:?}'",
                expected,
                result,
                status_code,
                body.to_str_lossy(),
                model_id
            );
        }
    }
}
