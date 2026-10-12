//! 全局出站代理相关命令
//!
//! 只做「解析参数 → 调 `services::global_proxy`」。逻辑（含 `ProxyTestResult` 等
//! 返回类型）已搬到 `services/global_proxy.rs`，服务端的 `web/routes.rs` 走同一份
//! 实现：那个模块才编得进 server 构建，而这里的类型留在桌面层就谁也看不见。

use crate::services::global_proxy::{
    get_global_proxy_url as get_url, scan_local_proxies_blocking, set_global_proxy_url as set_url,
    test_proxy_url as test_url, upstream_proxy_status, DetectedProxy, ProxyTestResult,
    UpstreamProxyStatus,
};
use crate::store::AppState;

/// 获取全局代理 URL
///
/// 返回当前配置的代理 URL，null 表示直连。
#[tauri::command]
pub fn get_global_proxy_url(state: tauri::State<'_, AppState>) -> Result<Option<String>, String> {
    get_url(state.inner())
}

/// 设置全局代理 URL
///
/// - 传入非空字符串：启用代理
/// - 传入空字符串：清除代理（直连）
#[tauri::command]
pub fn set_global_proxy_url(state: tauri::State<'_, AppState>, url: String) -> Result<(), String> {
    set_url(state.inner(), url)
}

/// 测试代理连接
///
/// 通过指定的代理 URL 发送测试请求，返回连接结果和延迟。
/// 使用多个测试目标，任一成功即认为代理可用。
#[tauri::command]
pub async fn test_proxy_url(url: String) -> Result<ProxyTestResult, String> {
    test_url(url).await
}

/// 获取当前出站代理状态
///
/// 返回当前是否启用了出站代理以及代理 URL。
#[tauri::command]
pub fn get_upstream_proxy_status() -> UpstreamProxyStatus {
    upstream_proxy_status()
}

/// 扫描本地代理
///
/// 检测常见端口是否有代理服务在运行。
/// 使用异步任务避免阻塞 UI 线程。
#[tauri::command]
pub async fn scan_local_proxies() -> Vec<DetectedProxy> {
    tokio::task::spawn_blocking(scan_local_proxies_blocking)
        .await
        .unwrap_or_default()
}
