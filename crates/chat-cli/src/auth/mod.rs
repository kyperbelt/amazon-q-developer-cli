// Authentication now uses AWS IAM credentials exclusively
// Builder ID and IAM Identity Center login flows have been removed

pub mod builder_id;

/// Check if AWS credentials are available
/// This is a lightweight check that only verifies a credentials provider exists
pub async fn is_logged_in() -> bool {
    aws_config::load_from_env()
        .await
        .credentials_provider()
        .is_some()
}
