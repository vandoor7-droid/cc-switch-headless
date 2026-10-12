//! WebDAV 同步的流程胶水：读写设置、合并密码、串行化、把失败记回设置。
//!
//! 这些原先住在 `commands/webdav_sync.rs`，而那个模块只在 `desktop` feature 下
//! 编译——服务端要发同样的命令，就必须能看见同一份实现。它们不碰 Tauri，
//! 所以搬到这里：两边共用，语义不可能各走各的。
//!
//! 值得说明的是**全局同步互斥锁不在这儿**：它一直在 `services::webdav_sync`，
//! 桌面命令与这里都只是去取它。那个锁是单例（有测试盯着），两个恢复同时跑会把
//! 文件写成半新半旧，所以它只该有一处定义。

use crate::error::AppError;
use crate::services::webdav_sync as webdav_sync_service;
use crate::settings::{self, WebDavSyncSettings};

/// 与后端无关的收尾胶水，WebDAV 与 S3 共用——见 `services::sync_support`。
pub use crate::services::sync_support::map_sync_result;

/// 把一次失败记回设置，供界面上的「上次错误」显示。
pub fn persist_sync_error(settings: &mut WebDavSyncSettings, error: &AppError, source: &str) {
    settings.status.last_error = Some(error.to_string());
    settings.status.last_error_source = Some(source.to_string());
    let _ = settings::update_webdav_sync_status(settings.status.clone());
}

fn not_configured_error() -> String {
    AppError::localized(
        "webdav.sync.not_configured",
        "未配置 WebDAV 同步",
        "WebDAV sync is not configured.",
    )
    .to_string()
}

fn disabled_error() -> String {
    AppError::localized(
        "webdav.sync.disabled",
        "WebDAV 同步未启用",
        "WebDAV sync is disabled.",
    )
    .to_string()
}

/// 取一份**已启用**的同步设置；未配置或未启用都是明确错误，不是空表。
pub fn require_enabled_settings() -> Result<WebDavSyncSettings, String> {
    let settings = settings::get_webdav_sync_settings().ok_or_else(not_configured_error)?;
    if !settings.enabled {
        return Err(disabled_error());
    }
    Ok(settings)
}

/// 决定一次请求用哪个密码。
///
/// 表单把密码框留空有两种意思：「没改过」和「清空」。`preserve_empty_password`
/// 为真时按前者处理，从已存设置里取回原值；显式传了值就用传来的。
///
/// 这是敏感逻辑：判断错会让一次普通的改名操作把 WebDAV 密码清掉。
pub fn resolve_password_for_request(
    mut incoming: WebDavSyncSettings,
    existing: Option<WebDavSyncSettings>,
    preserve_empty_password: bool,
) -> WebDavSyncSettings {
    if let Some(existing_settings) = existing {
        if preserve_empty_password && incoming.password.is_empty() {
            incoming.password = existing_settings.password;
        }
    }
    incoming
}

/// 在全局同步锁里跑一次操作。
pub async fn run_with_lock<T, Fut>(operation: Fut) -> Result<T, AppError>
where
    Fut: std::future::Future<Output = Result<T, AppError>>,
{
    webdav_sync_service::run_with_sync_lock(operation).await
}

/// 下载 + 下载后的投影，整段持锁。
///
/// 投影（把快照恢复成各处 live 文件）必须和下载在**同一次**持锁里做完：
/// 否则另一轮恢复可能在「库已应用、投影还没跑完」的窗口里插进来。
pub async fn run_download_with_lock<T, U, DownloadFut, Project, ProjectFut>(
    download: DownloadFut,
    project: Project,
) -> Result<U, AppError>
where
    DownloadFut: std::future::Future<Output = Result<T, AppError>>,
    Project: FnOnce(T) -> ProjectFut,
    ProjectFut: std::future::Future<Output = Result<U, AppError>>,
{
    run_with_lock(async {
        let result = {
            let _auto_sync_suppression =
                crate::services::webdav_auto_sync::AutoSyncSuppressionGuard::new();
            download.await?
        };
        project(result).await
    })
    .await
}
