// Builder ID authentication has been removed
// This file contains minimal stubs for compatibility during migration

use crate::database::Database;

/// Always returns false - IDC users are no longer supported
pub async fn is_idc_user(_database: &Database) -> eyre::Result<bool> {
    Ok(false)
}

/// Returns None for both start_url and region - no longer used
pub async fn get_start_url_and_region(_database: &Database) -> (Option<String>, Option<String>) {
    (None, None)
}

/// Stub bearer resolver - no longer used
#[derive(Debug, Clone, Copy)]
pub struct BearerResolver;
