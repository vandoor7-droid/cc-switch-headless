//! Codex OAuth state and Codex-specific commands.
//!
//! 查询实现（解析账号、取 token、查额度、拉模型）在 `services::codex_oauth_queries`
//! 里，服务端的 `web/routes.rs` 走同一份：它不在 desktop 门控里，语义因此不可能
//! 两边各走各的。
//!
//! 这里额外做两件桌面专属的收尾：把额度按账号记进 `usage_cache`、并刷新托盘
//! （托盘的额度文字读的就是这份缓存）。`tray.rs` 会**直接调用**这里的函数，
//! 所以签名不能简化掉这两个参数。

use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
use crate::services::model_fetch::FetchedModel;
use crate::services::subscription::SubscriptionQuota;
use std::sync::Arc;
use tauri::State;

pub struct CodexOAuthState(pub Arc<CodexOAuthManager>);

/// 查询 Codex OAuth (ChatGPT Plus/Pro) 订阅额度。
///
/// 与 `get_xai_oauth_quota` 同一语义：取不到 access_token 时返回一条带
/// `Expired` 状态的快照（界面把额度收起并提示重新登录），而不是把一次读变成红字。
///
/// 额度按**解析出的账号**记进缓存，哪怕请求期间默认账号换人也记对地方；只有
/// 查询成功才记，瞬时传输失败保留上一份快照——和前端 react-query「reject 保留
/// 上次 data」一个意思。
#[tauri::command(rename_all = "camelCase")]
pub async fn get_codex_oauth_quota(
    app: tauri::AppHandle,
    app_state: State<'_, crate::store::AppState>,
    account_id: Option<String>,
    state: State<'_, CodexOAuthState>,
) -> Result<SubscriptionQuota, String> {
    let manager = &state.0;
    let resolved =
        crate::services::codex_oauth_queries::resolve_account(manager, account_id.clone()).await;
    let result = crate::services::codex_oauth_queries::query_quota(manager, account_id).await;
    if let Ok(quota) = &result {
        if let Some(id) = resolved {
            app_state.usage_cache.put_codex_oauth(id, quota.clone());
            crate::tray::schedule_tray_refresh(&app);
        }
    }
    result
}

/// 获取 Codex OAuth (ChatGPT Plus/Pro) 可用模型列表。
///
/// ChatGPT Codex 反代用 `chatgpt.com/backend-api/codex/*`，不是 OpenAI 兼容的
/// `/v1/models`；这里复用托管 OAuth 账号的 access_token 直接读 Codex 后端。
#[tauri::command(rename_all = "camelCase")]
pub async fn get_codex_oauth_models(
    account_id: Option<String>,
    state: State<'_, CodexOAuthState>,
) -> Result<Vec<FetchedModel>, String> {
    crate::services::codex_oauth_queries::list_models(&state.0, account_id).await
}
