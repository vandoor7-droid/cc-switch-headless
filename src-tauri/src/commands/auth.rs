use tauri::State;

use crate::app_config::AppType;
use crate::commands::codex_oauth::CodexOAuthState;
use crate::commands::copilot::CopilotAuthState;
use crate::commands::xai_oauth::XaiOAuthState;
use crate::proxy::providers::codex_oauth_auth::CodexOAuthError;
use crate::proxy::providers::copilot_auth::CopilotAuthError;
use crate::proxy::providers::xai_oauth_auth::XaiOAuthError;
use crate::store::AppState;
// 形状与映射在 `services::managed_auth`，服务端的 `web/routes.rs` 共用同一份：
// 它不在 desktop 门控里，两边的序列化形状因此不可能漂。
pub use crate::services::managed_auth::{
    ensure_auth_provider, map_account, map_device_code_response, map_xai_account,
    ManagedAuthAccount, ManagedAuthDeviceCodeResponse, ManagedAuthStatus,
    AUTH_PROVIDER_CODEX_OAUTH, AUTH_PROVIDER_GITHUB_COPILOT, AUTH_PROVIDER_XAI_OAUTH,
};

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_start_login(
    auth_provider: String,
    github_domain: Option<String>,
    target_account_id: Option<String>,
    copilot_state: State<'_, CopilotAuthState>,
    codex_state: State<'_, CodexOAuthState>,
    xai_state: State<'_, XaiOAuthState>,
) -> Result<ManagedAuthDeviceCodeResponse, String> {
    let auth_provider = ensure_auth_provider(&auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            if target_account_id.is_some() {
                return Err("Targeted re-authentication is only supported for Codex OAuth".into());
            }
            let auth_manager = copilot_state.0.read().await;
            let response = auth_manager
                .start_device_flow(github_domain.as_deref())
                .await
                .map_err(|e| e.to_string())?;
            Ok(map_device_code_response(auth_provider, response))
        }
        AUTH_PROVIDER_CODEX_OAUTH => {
            let auth_manager = &codex_state.0;
            let response = auth_manager
                .start_device_flow(target_account_id.as_deref())
                .await
                .map_err(|e| e.to_string())?;
            Ok(map_device_code_response(auth_provider, response))
        }
        AUTH_PROVIDER_XAI_OAUTH => {
            if target_account_id.is_some() {
                return Err("Targeted re-authentication is only supported for Codex OAuth".into());
            }
            let auth_manager = xai_state.0.read().await;
            let response = auth_manager
                .start_device_flow()
                .await
                .map_err(|e| e.to_string())?;
            Ok(map_device_code_response(auth_provider, response))
        }
        _ => unreachable!(),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_poll_for_account(
    auth_provider: String,
    device_code: String,
    github_domain: Option<String>,
    app_state: State<'_, AppState>,
    copilot_state: State<'_, CopilotAuthState>,
    codex_state: State<'_, CodexOAuthState>,
    xai_state: State<'_, XaiOAuthState>,
) -> Result<Option<ManagedAuthAccount>, String> {
    let auth_provider = ensure_auth_provider(&auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            let auth_manager = copilot_state.0.write().await;
            match auth_manager
                .poll_for_token(&device_code, github_domain.as_deref())
                .await
            {
                Ok(account) => {
                    let default_account_id = auth_manager.get_status().await.default_account_id;
                    Ok(account.map(|account| {
                        map_account(auth_provider, account, default_account_id.as_deref())
                    }))
                }
                Err(CopilotAuthError::AuthorizationPending) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }
        AUTH_PROVIDER_CODEX_OAUTH => {
            let auth_manager = &codex_state.0;
            match auth_manager
                .poll_for_token(&device_code, || async {
                    app_state
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
                Err(CodexOAuthError::AuthorizationPending) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }
        AUTH_PROVIDER_XAI_OAUTH => {
            let auth_manager = xai_state.0.write().await;
            match auth_manager.poll_for_token(&device_code).await {
                Ok(account) => {
                    let default_account_id = auth_manager.get_status().await.default_account_id;
                    Ok(account
                        .map(|account| map_xai_account(account, default_account_id.as_deref())))
                }
                Err(XaiOAuthError::AuthorizationPending) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }
        _ => unreachable!(),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_cancel_login(
    auth_provider: String,
    device_code: String,
    codex_state: State<'_, CodexOAuthState>,
) -> Result<bool, String> {
    let auth_provider = ensure_auth_provider(&auth_provider)?;
    if auth_provider != AUTH_PROVIDER_CODEX_OAUTH {
        return Err("Login cancellation is only supported for Codex OAuth".to_string());
    }
    Ok(codex_state.0.cancel_device_flow(&device_code).await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_list_accounts(
    auth_provider: String,
    copilot_state: State<'_, CopilotAuthState>,
    codex_state: State<'_, CodexOAuthState>,
    xai_state: State<'_, XaiOAuthState>,
) -> Result<Vec<ManagedAuthAccount>, String> {
    let auth_provider = ensure_auth_provider(&auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            let auth_manager = copilot_state.0.read().await;
            let status = auth_manager.get_status().await;
            let default_account_id = status.default_account_id.clone();
            Ok(status
                .accounts
                .into_iter()
                .map(|account| map_account(auth_provider, account, default_account_id.as_deref()))
                .collect())
        }
        AUTH_PROVIDER_CODEX_OAUTH => {
            let auth_manager = &codex_state.0;
            let status = auth_manager.get_status().await;
            let default_account_id = status.default_account_id.clone();
            Ok(status
                .accounts
                .into_iter()
                .map(|account| map_account(auth_provider, account, default_account_id.as_deref()))
                .collect())
        }
        AUTH_PROVIDER_XAI_OAUTH => {
            let auth_manager = xai_state.0.read().await;
            let status = auth_manager.get_status().await;
            let default_account_id = status.default_account_id.clone();
            Ok(status
                .accounts
                .into_iter()
                .map(|account| map_xai_account(account, default_account_id.as_deref()))
                .collect())
        }
        _ => unreachable!(),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_get_status(
    auth_provider: String,
    copilot_state: State<'_, CopilotAuthState>,
    codex_state: State<'_, CodexOAuthState>,
    xai_state: State<'_, XaiOAuthState>,
) -> Result<ManagedAuthStatus, String> {
    let auth_provider = ensure_auth_provider(&auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            let auth_manager = copilot_state.0.read().await;
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
            let auth_manager = &codex_state.0;
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
            let auth_manager = xai_state.0.read().await;
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
        _ => unreachable!(),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_remove_account(
    auth_provider: String,
    account_id: String,
    app_state: State<'_, AppState>,
    copilot_state: State<'_, CopilotAuthState>,
    xai_state: State<'_, XaiOAuthState>,
) -> Result<(), String> {
    let auth_provider = ensure_auth_provider(&auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            let auth_manager = copilot_state.0.write().await;
            auth_manager
                .remove_account(&account_id)
                .await
                .map_err(|e| e.to_string())
        }
        AUTH_PROVIDER_CODEX_OAUTH => {
            remove_codex_oauth_account_with_switch_lock(app_state.inner(), &account_id).await
        }
        AUTH_PROVIDER_XAI_OAUTH => {
            let auth_manager = xai_state.0.write().await;
            auth_manager
                .remove_account(&account_id)
                .await
                .map_err(|e| e.to_string())
        }
        _ => unreachable!(),
    }
}

pub(crate) async fn remove_codex_oauth_account_with_switch_lock(
    app_state: &AppState,
    account_id: &str,
) -> Result<(), String> {
    // Serialize Auth Center credential deletion with managed provider
    // add/update/switch/hot-switch. Otherwise a switch that already preflighted
    // a bundle could recreate auth.json after removal.
    let _switch_guard = app_state
        .proxy_service
        .lock_switch_for_app(AppType::Codex.as_str())
        .await;
    app_state
        .codex_oauth_manager
        .remove_account(account_id)
        .await
        .map_err(|error| error.to_string())?;
    app_state
        .db
        .unbind_codex_managed_accounts(&[account_id.to_string()])
        .map_err(|error| {
            format!("账号已删除，但 Codex 供应商解绑失败；请编辑供应商重新选择账号或切换供应商: {error}")
        })?;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_set_default_account(
    auth_provider: String,
    account_id: String,
    copilot_state: State<'_, CopilotAuthState>,
    codex_state: State<'_, CodexOAuthState>,
    xai_state: State<'_, XaiOAuthState>,
) -> Result<(), String> {
    let auth_provider = ensure_auth_provider(&auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            let auth_manager = copilot_state.0.write().await;
            auth_manager
                .set_default_account(&account_id)
                .await
                .map_err(|e| e.to_string())
        }
        AUTH_PROVIDER_CODEX_OAUTH => {
            let auth_manager = &codex_state.0;
            auth_manager
                .set_default_account(&account_id)
                .await
                .map_err(|e| e.to_string())
        }
        AUTH_PROVIDER_XAI_OAUTH => {
            let auth_manager = xai_state.0.write().await;
            auth_manager
                .set_default_account(&account_id)
                .await
                .map_err(|e| e.to_string())
        }
        _ => unreachable!(),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn auth_logout(
    auth_provider: String,
    app_state: State<'_, AppState>,
    copilot_state: State<'_, CopilotAuthState>,
    xai_state: State<'_, XaiOAuthState>,
) -> Result<(), String> {
    let auth_provider = ensure_auth_provider(&auth_provider)?;
    match auth_provider {
        AUTH_PROVIDER_GITHUB_COPILOT => {
            let auth_manager = copilot_state.0.write().await;
            auth_manager.clear_auth().await.map_err(|e| e.to_string())
        }
        AUTH_PROVIDER_CODEX_OAUTH => logout_codex_oauth_with_switch_lock(app_state.inner()).await,
        AUTH_PROVIDER_XAI_OAUTH => {
            let auth_manager = xai_state.0.write().await;
            auth_manager.clear_auth().await.map_err(|e| e.to_string())
        }
        _ => unreachable!(),
    }
}

pub(crate) async fn logout_codex_oauth_with_switch_lock(
    app_state: &AppState,
) -> Result<(), String> {
    let _switch_guard = app_state
        .proxy_service
        .lock_switch_for_app(AppType::Codex.as_str())
        .await;
    // Only this device's accounts: rows synced from another device stay bound there.
    let removed: Vec<String> = app_state
        .codex_oauth_manager
        .list_accounts()
        .await
        .into_iter()
        .map(|account| account.id)
        .collect();
    app_state
        .codex_oauth_manager
        .clear_auth()
        .await
        .map_err(|error| error.to_string())?;
    app_state
        .db
        .unbind_codex_managed_accounts(&removed)
        .map_err(|error| {
            format!("托管账号已登出，但 Codex 供应商解绑失败；请编辑供应商重新选择账号或切换供应商: {error}")
        })?;
    Ok(())
}
