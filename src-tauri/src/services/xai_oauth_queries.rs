//! xAI (SuperGrok) 托管账号的查询：订阅额度与模型列表。
//!
//! 桌面命令层（`commands/xai_oauth.rs`）与服务端的 `web/routes.rs` 共用这里的
//! 实现。前者原先把这些逻辑连同私有助手一起放在 desktop 门控的文件里，服务端
//! 因此看不见；按 `web/routes.rs` 的约定搬来这里，两边都只做解析与调用。
//!
//! 管理器本身从 `proxy::oauth_registry` 拿，不从 Tauri 的 `.manage()` 容器拿
//! ——那个容器只在桌面构建里存在。`web/state.rs` 启动时同样会把
//! `XaiOAuthManager` 注册进 registry，所以两种构建拿到的是同一个实例。
//!
//! 解析账号的顺序固定为 **显式 > 默认账号 > 无账号**；无账号时额度查询返回
//! `not_found`（界面按"没有可用账号"显示），模型查询则报错（它没有"空列表"
//! 这个体面的退路，用户是明确要求拉模型的）。

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::proxy::providers::xai_oauth_auth::XaiOAuthManager;
use crate::services::model_fetch::FetchedModel;
use crate::services::subscription::{CredentialStatus, SubscriptionQuota};

/// xAI 的 API 根地址（SuperGrok 后端）。

#[derive(Debug, serde::Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
}

/// 解析最终使用的账号 ID：显式 > 默认账号 > 无账号。
async fn resolve_account(manager: &XaiOAuthManager, account_id: Option<String>) -> Option<String> {
    match account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => Some(id.to_string()),
        None => manager.default_account_id().await,
    }
}

/// 查询 xAI OAuth (SuperGrok) 订阅额度。
///
/// 取不到 access_token 时**不是错误**，而是返回一条带 `Expired` 状态的快照：
/// 界面据此把额度收起来并提示重新登录，而不是把一次读变成红字。
pub async fn query_quota(
    manager: &Arc<RwLock<XaiOAuthManager>>,
    account_id: Option<String>,
) -> Result<SubscriptionQuota, String> {
    let manager = manager.read().await;

    let Some(id) = resolve_account(&manager, account_id).await else {
        return Ok(SubscriptionQuota::not_found("xai_oauth"));
    };

    let token = match manager.get_valid_token_for_account(&id).await {
        Ok(token) => token,
        Err(error) => {
            return Ok(SubscriptionQuota::error(
                "xai_oauth",
                CredentialStatus::Expired,
                format!("xAI OAuth token unavailable: {error}"),
            ));
        }
    };

    crate::services::subscription_grok::query_grok_quota(
        &token,
        "xai_oauth",
        "Please re-login via cc-switch.",
    )
    .await
}

/// 获取 xAI OAuth (SuperGrok) 可用模型列表。
pub async fn list_models(
    manager: &Arc<RwLock<XaiOAuthManager>>,
    account_id: Option<String>,
) -> Result<Vec<FetchedModel>, String> {
    let manager = manager.read().await;
    let id = resolve_account(&manager, account_id)
        .await
        .ok_or_else(|| "No usable xAI account available".to_string())?;
    let token = manager
        .get_valid_token_for_account(&id)
        .await
        .map_err(|error| format!("xAI OAuth token unavailable: {error}"))?;

    let response = crate::proxy::http_client::get()
        // 用共享常量：它是 `https://api.x.ai/v1`，与转发链路同一处定义，
        // 自己再拼一份根地址会得到 `/models` 而不是 `/v1/models`。
        .get(format!(
            "{}/models",
            crate::proxy::providers::XAI_API_BASE_URL
        ))
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|error| format!("xAI models request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("xAI models request failed: HTTP {status}"));
    }

    let payload: ModelsResponse = response
        .json()
        .await
        .map_err(|_| "xAI models response was not valid JSON".to_string())?;
    let mut models: Vec<FetchedModel> = payload
        .data
        .into_iter()
        .map(|model| FetchedModel {
            id: model.id,
            owned_by: model.owned_by,
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}
