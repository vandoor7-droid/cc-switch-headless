//! Codex OAuth (ChatGPT Plus/Pro) 托管账号的查询：订阅额度与模型列表。
//!
//! 与 `services::xai_oauth_queries` 同构：桌面命令层（`commands/codex_oauth.rs`）
//! 与服务端的 `web/routes.rs` 共用这里的实现，前者只做「取管理器 → 调用」。
//!
//! 管理器从 `proxy::oauth_registry` 取，不从 Tauri 的 `.manage()` 容器拿——
//! 后者只在桌面构建里存在；`web/state.rs` 启动时同样会把它注册进 registry。
//!
//! 解析账号的顺序固定为 **显式 > 默认账号 > 无账号**：额度查询在无账号时返回
//! `not_found`（界面静默不渲染），模型查询则报错——用户是明确要求拉模型的，
//! 返回空列表会像「一个模型都没有」。

use std::sync::Arc;

use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
use crate::services::model_fetch::FetchedModel;
use crate::services::subscription::{CredentialStatus, SubscriptionQuota};

/// 解析最终使用的账号 ID：显式 > 默认账号 > 无账号。
///
/// 公开是因为桌面侧要把查到的额度按账号记进 usage_cache，需要知道最终用了
/// 哪个账号——那个 id 是缓存的键。
pub async fn resolve_account(
    manager: &CodexOAuthManager,
    account_id: Option<String>,
) -> Option<String> {
    match account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => Some(id.to_string()),
        None => manager.default_account_id().await,
    }
}

/// 查询 Codex OAuth (ChatGPT Plus/Pro) 订阅额度。
///
/// 与 `get_xai_oauth_quota` 同一语义：取不到 token **不是错误**，而是返回一条带
/// `Expired` 状态的快照，界面据此把额度收起并提示重新登录。
///
/// 瞬时传输失败以 `Err` 传播（前端 reject → retry 并保留上次成功值），只有
/// 认证/HTTP 失败才替换成错误快照——这个区分让一次网络抖动不会看起来像密钥过期。
pub async fn query_quota(
    manager: &Arc<CodexOAuthManager>,
    account_id: Option<String>,
) -> Result<SubscriptionQuota, String> {
    let Some(id) = resolve_account(manager.as_ref(), account_id).await else {
        return Ok(SubscriptionQuota::not_found("codex_oauth"));
    };

    let token = match manager.get_valid_token_for_account(&id).await {
        Ok(token) => token,
        Err(error) => {
            return Ok(SubscriptionQuota::error(
                "codex_oauth",
                CredentialStatus::Expired,
                format!("Codex OAuth token unavailable: {error}"),
            ));
        }
    };
    let chatgpt_account_id = manager
        .chatgpt_account_id_for_account(&id)
        .await
        .map_err(|error| error.to_string())?;

    crate::services::subscription::query_codex_quota(
        &token,
        Some(&chatgpt_account_id),
        "codex_oauth",
        "Codex OAuth access token expired or rejected. Please re-login via cc-switch.",
    )
    .await
}

/// 获取 Codex OAuth (ChatGPT Plus/Pro) 可用模型列表。
///
/// ChatGPT Codex 反代用 `chatgpt.com/backend-api/codex/*`，不是 OpenAI 兼容的
/// `/v1/models`。这里复用托管 OAuth 账号的 access_token，直接读 Codex 后端暴露
/// 的模型列表端点。
pub async fn list_models(
    manager: &Arc<CodexOAuthManager>,
    account_id: Option<String>,
) -> Result<Vec<FetchedModel>, String> {
    let id = resolve_account(manager.as_ref(), account_id)
        .await
        .ok_or_else(|| "No ChatGPT account available".to_string())?;

    let token = manager
        .get_valid_token_for_account(&id)
        .await
        .map_err(|error| format!("Codex OAuth token unavailable: {error}"))?;
    let chatgpt_account_id = manager
        .chatgpt_account_id_for_account(&id)
        .await
        .map_err(|error| error.to_string())?;

    crate::services::codex_oauth_models::fetch_models_with_token(&token, &chatgpt_account_id).await
}
