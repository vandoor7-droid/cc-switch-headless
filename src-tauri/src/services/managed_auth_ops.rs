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

// ============================================================================
// 设备码登录
//
// 三条命令共用一个细节：**轮询时「还没授权」不是错误**。三种管理器各自的
// `AuthorizationPending` 都映射成 `Ok(None)`，前端据此继续轮询；只有真正的
// 失败才变成 Err。把它当成错误会让登录流程在用户还没扫码时就红掉。
//
// Codex 的 `poll_for_token` 还要一个 `before_commit` 钩子：提交 token 前先拿
// 切换锁，避免与托管供应商的增删改撞在一起。桌面命令传的是同一个闭包。
// ============================================================================

/// 发起设备码登录。
///
/// 只有 Codex 支持「指定账号重新登录」；另外两家传了 `target_account_id`
/// 就明确拒绝，而不是悄悄忽略。
pub async fn start_login(
    managers: &Managers,
    auth_provider: &str,
    github_domain: Option<&str>,
    target_account_id: Option<&str>,
) -> Result<crate::services::managed_auth::ManagedAuthDeviceCodeResponse, String> {
    use crate::services::managed_auth::{
        ensure_auth_provider, map_device_code_response, AUTH_PROVIDER_CODEX_OAUTH,
        AUTH_PROVIDER_GITHUB_COPILOT, AUTH_PROVIDER_XAI_OAUTH,
    };
    let auth_provider = ensure_auth_provider(auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            if target_account_id.is_some() {
                return Err("Targeted re-authentication is only supported for Codex OAuth".into());
            }
            let auth_manager = managers
                .copilot
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?
                .read()
                .await;
            let response = auth_manager
                .start_device_flow(github_domain)
                .await
                .map_err(|error| error.to_string())?;
            Ok(map_device_code_response(auth_provider, response))
        }
        AUTH_PROVIDER_CODEX_OAUTH => {
            let auth_manager = managers
                .codex
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?;
            let response = auth_manager
                .start_device_flow(target_account_id)
                .await
                .map_err(|error| error.to_string())?;
            Ok(map_device_code_response(auth_provider, response))
        }
        AUTH_PROVIDER_XAI_OAUTH => {
            if target_account_id.is_some() {
                return Err("Targeted re-authentication is only supported for Codex OAuth".into());
            }
            let auth_manager = managers
                .xai
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?
                .read()
                .await;
            let response = auth_manager
                .start_device_flow()
                .await
                .map_err(|error| error.to_string())?;
            Ok(map_device_code_response(auth_provider, response))
        }
        other => Err(format!("Unsupported auth provider: {other}")),
    }
}

/// 轮询设备码登录；还没授权时返回 `None`，见文件头。
pub async fn poll_for_account(
    state: &AppState,
    managers: &Managers,
    auth_provider: &str,
    device_code: &str,
    github_domain: Option<&str>,
) -> Result<Option<crate::services::managed_auth::ManagedAuthAccount>, String> {
    use crate::services::managed_auth::{
        ensure_auth_provider, map_account, map_xai_account, AUTH_PROVIDER_CODEX_OAUTH,
        AUTH_PROVIDER_GITHUB_COPILOT, AUTH_PROVIDER_XAI_OAUTH,
    };
    let auth_provider = ensure_auth_provider(auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            let auth_manager = managers
                .copilot
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?
                .write()
                .await;
            match auth_manager
                .poll_for_token(device_code, github_domain)
                .await
            {
                Ok(account) => {
                    let default_account_id = auth_manager.get_status().await.default_account_id;
                    Ok(account.map(|account| {
                        map_account(auth_provider, account, default_account_id.as_deref())
                    }))
                }
                Err(
                    crate::proxy::providers::copilot_auth::CopilotAuthError::AuthorizationPending,
                ) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }
        AUTH_PROVIDER_CODEX_OAUTH => {
            let auth_manager = managers
                .codex
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?;
            match auth_manager
                .poll_for_token(device_code, || async {
                    state
                        .proxy_service
                        .lock_switch_for_app(AppType::Codex.as_str())
                        .await
                })
                .await
            {
                Ok(account) => {
                    let default_account_id = auth_manager.get_status().await.default_account_id;
                    Ok(account.map(|account| {
                        map_account(auth_provider, account, default_account_id.as_deref())
                    }))
                }
                Err(crate::proxy::providers::codex_oauth_auth::CodexOAuthError::AuthorizationPending) => {
                    Ok(None)
                }
                Err(error) => Err(error.to_string()),
            }
        }
        AUTH_PROVIDER_XAI_OAUTH => {
            let auth_manager = managers
                .xai
                .as_ref()
                .ok_or_else(|| missing(auth_provider))?
                .write()
                .await;
            match auth_manager.poll_for_token(device_code).await {
                Ok(account) => {
                    let default_account_id = auth_manager.get_status().await.default_account_id;
                    Ok(account
                        .map(|account| map_xai_account(account, default_account_id.as_deref())))
                }
                Err(
                    crate::proxy::providers::xai_oauth_auth::XaiOAuthError::AuthorizationPending,
                ) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }
        other => Err(format!("Unsupported auth provider: {other}")),
    }
}

/// 取消设备码登录。只有 Codex 支持，其余明确拒绝而不是静默成功。
pub async fn cancel_login(
    managers: &Managers,
    auth_provider: &str,
    device_code: &str,
) -> Result<bool, String> {
    use crate::services::managed_auth::{ensure_auth_provider, AUTH_PROVIDER_CODEX_OAUTH};
    let auth_provider = ensure_auth_provider(auth_provider)?;
    if auth_provider != AUTH_PROVIDER_CODEX_OAUTH {
        return Err("Login cancellation is only supported for Codex OAuth".to_string());
    }
    let auth_manager = managers
        .codex
        .as_ref()
        .ok_or_else(|| missing(auth_provider))?;
    Ok(auth_manager.cancel_device_flow(device_code).await)
}
