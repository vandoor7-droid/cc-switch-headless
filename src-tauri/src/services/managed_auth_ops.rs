//! 托管账号的管理操作：查状态、列账号、删账号、设默认、注销。
//!
//! 与 `services::managed_auth`（形状与映射）分工：那边管「一条账号在接口上长
//! 什么样」，这边管「怎么把它改掉」。桌面命令与 `web/routes.rs` 共用这里的实现。
//!
//! 三个 provider 的差异都集中在 Codex 一处：它要**先拿切换锁**再动凭据。
//! `remove_codex_oauth_account_with_switch_lock` 与
//! `logout_codex_oauth_with_switch_lock` 因此不是普通调用——漏掉那把锁，
//! 一个已经预检过凭据束的切换可能在删除之后把 auth.json 重新写出来。
//!
//! 这些操作目前只有服务端的 `web/routes.rs` 调用（桌面命令还在用自己的匹配
//! 块，见下方 TODO）。因此在**不构建 server** 的目标里它们是死代码，
//! 标注用 `cfg_attr` 精确表达这一点，而不是笼统地 `allow(dead_code)`——
//! 后者会连真正没人用的东西一起放过。
//!
//! 管理器由调用方提供：桌面从 Tauri 的 `.manage()` 容器取，服务端从
//! `proxy::oauth_registry` 取。两者在启动时注册的是同一批实例。

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::app_config::AppType;
use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
use crate::proxy::providers::copilot_auth::CopilotAuthManager;
use crate::proxy::providers::xai_oauth_auth::XaiOAuthManager;
use crate::services::managed_auth::{
    ensure_auth_provider, map_account, map_xai_account, ManagedAuthAccount, ManagedAuthStatus,
    AUTH_PROVIDER_CODEX_OAUTH, AUTH_PROVIDER_GITHUB_COPILOT, AUTH_PROVIDER_XAI_OAUTH,
};
use crate::store::AppState;

/// 三种管理器，按 provider 取用。
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub struct Managers {
    pub copilot: Option<Arc<RwLock<CopilotAuthManager>>>,
    pub codex: Option<Arc<CodexOAuthManager>>,
    pub xai: Option<Arc<RwLock<XaiOAuthManager>>>,
}

fn missing(provider: &str) -> String {
    format!("No {provider} auth manager registered")
}

/// 列出某个 provider 的托管账号。
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub async fn list_accounts(
    managers: &Managers,
    auth_provider: &str,
) -> Result<Vec<ManagedAuthAccount>, String> {
    let auth_provider = ensure_auth_provider(auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            let auth_manager = managers
                .copilot
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?
                .read()
                .await;
            let status = auth_manager.get_status().await;
            let default_account_id = status.default_account_id.clone();
            Ok(status
                .accounts
                .into_iter()
                .map(|account| map_account(auth_provider, account, default_account_id.as_deref()))
                .collect())
        }
        AUTH_PROVIDER_CODEX_OAUTH => {
            let auth_manager = managers
                .codex
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?;
            let status = auth_manager.get_status().await;
            let default_account_id = status.default_account_id.clone();
            Ok(status
                .accounts
                .into_iter()
                .map(|account| map_account(auth_provider, account, default_account_id.as_deref()))
                .collect())
        }
        AUTH_PROVIDER_XAI_OAUTH => {
            let auth_manager = managers
                .xai
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?
                .read()
                .await;
            let status = auth_manager.get_status().await;
            let default_account_id = status.default_account_id.clone();
            Ok(status
                .accounts
                .into_iter()
                .map(|account| map_xai_account(account, default_account_id.as_deref()))
                .collect())
        }
        other => Err(format!("Unsupported auth provider: {other}")),
    }
}

/// 取某个 provider 的登录状态与账号。
///
/// Codex 没有 `migration_error` 这个概念，一律 `None`——它是 Copilot 迁移路径
/// 独有的，别当成「三家都有只是没值」。
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub async fn get_status(
    managers: &Managers,
    auth_provider: &str,
) -> Result<ManagedAuthStatus, String> {
    let auth_provider = ensure_auth_provider(auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            let auth_manager = managers
                .copilot
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?
                .read()
                .await;
            let status = auth_manager.get_status().await;
            let default_account_id = status.default_account_id.clone();
            Ok(ManagedAuthStatus {
                provider: auth_provider.to_string(),
                authenticated: status.authenticated,
                default_account_id: default_account_id.clone(),
                migration_error: status.migration_error,
                accounts: status
                    .accounts
                    .into_iter()
                    .map(|account| {
                        map_account(auth_provider, account, default_account_id.as_deref())
                    })
                    .collect(),
            })
        }
        AUTH_PROVIDER_CODEX_OAUTH => {
            let auth_manager = managers
                .codex
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?;
            let status = auth_manager.get_status().await;
            let default_account_id = status.default_account_id.clone();
            Ok(ManagedAuthStatus {
                provider: auth_provider.to_string(),
                authenticated: status.authenticated,
                default_account_id: default_account_id.clone(),
                migration_error: None,
                accounts: status
                    .accounts
                    .into_iter()
                    .map(|account| {
                        map_account(auth_provider, account, default_account_id.as_deref())
                    })
                    .collect(),
            })
        }
        AUTH_PROVIDER_XAI_OAUTH => {
            let auth_manager = managers
                .xai
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?
                .read()
                .await;
            let status = auth_manager.get_status().await;
            let default_account_id = status.default_account_id.clone();
            Ok(ManagedAuthStatus {
                provider: auth_provider.to_string(),
                authenticated: status.authenticated,
                default_account_id: default_account_id.clone(),
                migration_error: None,
                accounts: status
                    .accounts
                    .into_iter()
                    .map(|account| map_xai_account(account, default_account_id.as_deref()))
                    .collect(),
            })
        }
        other => Err(format!("Unsupported auth provider: {other}")),
    }
}

/// 设默认账号。
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub async fn set_default_account(
    managers: &Managers,
    auth_provider: &str,
    account_id: &str,
) -> Result<(), String> {
    let auth_provider = ensure_auth_provider(auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => managers
            .copilot
            .as_ref()
            .ok_or_else(|| missing(auth_provider))?
            .write()
            .await
            .set_default_account(account_id)
            .await
            .map_err(|error| error.to_string()),
        AUTH_PROVIDER_CODEX_OAUTH => managers
            .codex
            .as_ref()
            .ok_or_else(|| missing(auth_provider))?
            .set_default_account(account_id)
            .await
            .map_err(|error| error.to_string()),
        AUTH_PROVIDER_XAI_OAUTH => managers
            .xai
            .as_ref()
            .ok_or_else(|| missing(auth_provider))?
            .write()
            .await
            .set_default_account(account_id)
            .await
            .map_err(|error| error.to_string()),
        other => Err(format!("Unsupported auth provider: {other}")),
    }
}

/// 删除一个托管账号。Codex 走带锁的版本，见文件头。
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub async fn remove_account(
    state: &AppState,
    managers: &Managers,
    auth_provider: &str,
    account_id: &str,
) -> Result<(), String> {
    let auth_provider = ensure_auth_provider(auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => managers
            .copilot
            .as_ref()
            .ok_or_else(|| missing(auth_provider))?
            .write()
            .await
            .remove_account(account_id)
            .await
            .map_err(|error| error.to_string()),
        AUTH_PROVIDER_CODEX_OAUTH => {
            remove_codex_oauth_account_with_switch_lock(state, account_id).await
        }
        AUTH_PROVIDER_XAI_OAUTH => managers
            .xai
            .as_ref()
            .ok_or_else(|| missing(auth_provider))?
            .write()
            .await
            .remove_account(account_id)
            .await
            .map_err(|error| error.to_string()),
        other => Err(format!("Unsupported auth provider: {other}")),
    }
}

/// 注销某个 provider。Codex 走带锁的版本，见文件头。
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub async fn logout(
    state: &AppState,
    managers: &Managers,
    auth_provider: &str,
) -> Result<(), String> {
    let auth_provider = ensure_auth_provider(auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => managers
            .copilot
            .as_ref()
            .ok_or_else(|| missing(auth_provider))?
            .write()
            .await
            .clear_auth()
            .await
            .map_err(|error| error.to_string()),
        AUTH_PROVIDER_CODEX_OAUTH => logout_codex_oauth_with_switch_lock(state).await,
        AUTH_PROVIDER_XAI_OAUTH => managers
            .xai
            .as_ref()
            .ok_or_else(|| missing(auth_provider))?
            .write()
            .await
            .clear_auth()
            .await
            .map_err(|error| error.to_string()),
        other => Err(format!("Unsupported auth provider: {other}")),
    }
}

/// 删掉一个 Codex 托管账号（带切换锁）。
///
/// 把凭据删除与托管供应商的增/改/切换/热切换串行化：否则一个已经预检过凭据束的
/// 切换可能在删除之后把 auth.json 重新写出来。
pub async fn remove_codex_oauth_account_with_switch_lock(
    state: &AppState,
    account_id: &str,
) -> Result<(), String> {
    let _switch_guard = state
        .proxy_service
        .lock_switch_for_app(AppType::Codex.as_str())
        .await;
    state
        .codex_oauth_manager
        .remove_account(account_id)
        .await
        .map_err(|error| error.to_string())?;
    state
        .db
        .unbind_codex_managed_accounts(&[account_id.to_string()])
        .map_err(|error| {
            format!("账号已删除，但 Codex 供应商解绑失败；请编辑供应商重新挑选账号或切换供应商: {error}")
        })?;
    Ok(())
}

/// 注销 Codex 托管账号（带切换锁）。
///
/// 只清**本机**的账号：从别的设备同步来的行仍绑定在那台设备上。
pub async fn logout_codex_oauth_with_switch_lock(state: &AppState) -> Result<(), String> {
    let _switch_guard = state
        .proxy_service
        .lock_switch_for_app(AppType::Codex.as_str())
        .await;
    let removed: Vec<String> = state
        .codex_oauth_manager
        .list_accounts()
        .await
        .into_iter()
        .map(|account| account.id)
        .collect();
    state
        .codex_oauth_manager
        .clear_auth()
        .await
        .map_err(|error| error.to_string())?;
    state
        .db
        .unbind_codex_managed_accounts(&removed)
        .map_err(|error| {
            format!("托管账号已退出，但 Codex 供应商解绑失败；请编辑供应商重新挑选账号或切换供应商: {error}")
        })?;
    Ok(())
}
