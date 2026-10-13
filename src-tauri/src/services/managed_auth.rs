//! 托管账号（Copilot / Codex / xAI）的共享形状与映射。
//!
//! 这一层原先整个住在 `commands/auth.rs`，而那个模块只在 `desktop` feature 下
//! 编译——服务端要发同样的命令就看不见它。这些类型与映射不碰 Tauri，所以搬来
//! 这里：桌面命令与 `web/routes.rs` 共用同一份，序列化形状不可能两边各走各的。
//!
//! 三个 provider 的差异集中在两处，其余完全对称：
//!
//! - **Codex** 多一个 `reauth_required`：旧账号缺写入原生 Codex auth.json 所需
//!   的 id_token，界面上要提示重新登录。
//! - **xAI** 多一个 `requires_reauth`：refresh token 已失效，账号不能再用来发请求。
//!
//! 两个字段在对方的映射里都填 `false`，不能合并成一个——它们指的不是同一件事。

use crate::proxy::providers::copilot_auth::{GitHubAccount, GitHubDeviceCodeResponse};
use crate::proxy::providers::xai_oauth_auth::XaiOAuthAccount;

pub const AUTH_PROVIDER_GITHUB_COPILOT: &str = "github_copilot";
pub const AUTH_PROVIDER_CODEX_OAUTH: &str = "codex_oauth";
pub const AUTH_PROVIDER_XAI_OAUTH: &str = "xai_oauth";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedAuthAccount {
    pub id: String,
    pub provider: String,
    pub login: String,
    pub avatar_url: Option<String>,
    pub authenticated_at: i64,
    pub is_default: bool,
    pub github_domain: String,
    /// Codex 专用：旧账号缺少写入原生 Codex auth.json 所需的 id_token。
    pub reauth_required: bool,
    /// xAI 专用：refresh token 已失效，账号不可再用于请求。
    pub requires_reauth: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedAuthStatus {
    pub provider: String,
    pub authenticated: bool,
    pub default_account_id: Option<String>,
    pub migration_error: Option<String>,
    pub accounts: Vec<ManagedAuthAccount>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedAuthDeviceCodeResponse {
    pub provider: String,
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// 只认三个已知 provider；未知的明确拒绝，而不是静默落到某一个上。
pub fn ensure_auth_provider(auth_provider: &str) -> Result<&'static str, String> {
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => Ok(AUTH_PROVIDER_GITHUB_COPILOT),
        AUTH_PROVIDER_CODEX_OAUTH => Ok(AUTH_PROVIDER_CODEX_OAUTH),
        AUTH_PROVIDER_XAI_OAUTH => Ok(AUTH_PROVIDER_XAI_OAUTH),
        _ => Err(format!("Unsupported auth provider: {auth_provider}")),
    }
}

pub fn map_account(
    provider: &str,
    account: GitHubAccount,
    default_account_id: Option<&str>,
) -> ManagedAuthAccount {
    ManagedAuthAccount {
        is_default: default_account_id == Some(account.id.as_str()),
        reauth_required: account.reauth_required,
        requires_reauth: false,
        id: account.id,
        provider: provider.to_string(),
        login: account.login,
        avatar_url: account.avatar_url,
        authenticated_at: account.authenticated_at,
        github_domain: account.github_domain,
    }
}

pub fn map_xai_account(
    account: XaiOAuthAccount,
    default_account_id: Option<&str>,
) -> ManagedAuthAccount {
    ManagedAuthAccount {
        is_default: default_account_id == Some(account.id.as_str()),
        id: account.id,
        provider: AUTH_PROVIDER_XAI_OAUTH.to_string(),
        login: account.login,
        avatar_url: account.avatar_url,
        authenticated_at: account.authenticated_at,
        github_domain: account.github_domain,
        reauth_required: false,
        requires_reauth: account.requires_reauth,
    }
}

pub fn map_device_code_response(
    provider: &str,
    response: GitHubDeviceCodeResponse,
) -> ManagedAuthDeviceCodeResponse {
    ManagedAuthDeviceCodeResponse {
        provider: provider.to_string(),
        device_code: response.device_code,
        user_code: response.user_code,
        verification_uri: response.verification_uri,
        expires_in: response.expires_in,
        interval: response.interval,
    }
}
