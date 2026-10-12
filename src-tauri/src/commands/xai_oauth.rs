//! xAI OAuth state and xAI-specific commands.
//!
//! 只做「取管理器 → 调 `services::xai_oauth_queries`」。查询实现（解析账号、
//! 取 token、查额度、拉模型）在那个模块里，服务端的 `web/routes.rs` 走同一份：
//! 它不在 desktop 门控里，语义因此不可能两边各走各的。

use crate::proxy::providers::xai_oauth_auth::XaiOAuthManager;
use crate::services::model_fetch::FetchedModel;
use crate::services::subscription::SubscriptionQuota;
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

pub struct XaiOAuthState(pub Arc<RwLock<XaiOAuthManager>>);

/// 查询 xAI OAuth (SuperGrok) 订阅额度。
///
/// `account_id` 未指定时回退到 `XaiOAuthManager` 的默认账号；一个账号都没有时
/// 返回 `not_found`，前端 `SubscriptionQuotaView` 会静默不渲染。
#[tauri::command(rename_all = "camelCase")]
pub async fn get_xai_oauth_quota(
    account_id: Option<String>,
    state: State<'_, XaiOAuthState>,
) -> Result<SubscriptionQuota, String> {
    crate::services::xai_oauth_queries::query_quota(&state.0, account_id).await
}

/// 获取 xAI OAuth (SuperGrok) 可用模型列表。
#[tauri::command(rename_all = "camelCase")]
pub async fn get_xai_oauth_models(
    account_id: Option<String>,
    state: State<'_, XaiOAuthState>,
) -> Result<Vec<FetchedModel>, String> {
    crate::services::xai_oauth_queries::list_models(&state.0, account_id).await
}

/// `commands::provider` 的 official_subscription 分支（用量脚注/托盘路径）复用它。
/// xai_oauth 供应商的额度属绑定的 SuperGrok 账号，而非所在 app 的 CLI 凭据。
pub(crate) async fn query_xai_oauth_quota_for(
    state: &XaiOAuthState,
    account_id: Option<String>,
) -> Result<SubscriptionQuota, String> {
    crate::services::xai_oauth_queries::query_quota(&state.0, account_id).await
}
