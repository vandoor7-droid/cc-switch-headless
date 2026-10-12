//! S3 同步的流程胶水：读写设置、合并密钥、串行化、把失败记回设置。
//!
//! 与 `services::webdav_flow` 同构，只是换了后端类型。它原先住在
//! `commands/s3_sync.rs`，而那个模块只在 `desktop` feature 下编译——服务端要发
//! 同样的命令，就必须能看见同一份实现。它们不碰 Tauri，所以搬到这里。
//!
//! **全局同步互斥锁不在这儿**：它一直在 `services::s3_sync`，两边都只是去取。
//! 那个锁是单例，两个恢复同时跑会把文件写成半新半旧，所以它只该有一处定义。
//!
//! `map_sync_result` 来自 `services::sync_support`：它与后端无关，WebDAV 与
//! S3 共用，不该在两边各留一份。

use crate::error::AppError;
use crate::services::s3_sync as s3_sync_service;
use crate::settings::{self, S3SyncSettings};

pub use crate::services::sync_support::map_sync_result;

/// 把一次失败记回设置，供界面上的「上次错误」显示。
pub fn persist_sync_error(settings: &mut S3SyncSettings, error: &AppError, source: &str) {
    settings.status.last_error = Some(error.to_string());
    settings.status.last_error_source = Some(source.to_string());
    let _ = settings::update_s3_sync_status(settings.status.clone());
}

fn not_configured_error() -> String {
    AppError::localized(
        "s3.sync.not_configured",
        "未配置 S3 同步",
        "S3 sync is not configured.",
    )
    .to_string()
}

fn disabled_error() -> String {
    AppError::localized("s3.sync.disabled", "S3 同步未启用", "S3 sync is disabled.").to_string()
}

/// 取一份**已启用**的同步设置；未配置或未启用都是明确错误，不是空表。
pub fn require_enabled_settings() -> Result<S3SyncSettings, String> {
    let settings = settings::get_s3_sync_settings().ok_or_else(not_configured_error)?;
    if !settings.enabled {
        return Err(disabled_error());
    }
    Ok(settings)
}

/// 决定一次请求用哪个 secret。
///
/// 与 WebDAV 的密码合并同一个问题：表单留空有两种意思，「没改过」和「清空」。
/// `preserve_empty_secret` 为真时按前者处理，从已存设置里取回原值。
///
/// 判错的后果是把 S3 的 secret 清掉，而它和 access key 一起决定能不能同步。
pub fn resolve_secret_for_request(
    mut incoming: S3SyncSettings,
    existing: Option<S3SyncSettings>,
    preserve_empty_secret: bool,
) -> S3SyncSettings {
    if let Some(existing_settings) = existing {
        if preserve_empty_secret && incoming.secret_access_key.is_empty() {
            incoming.secret_access_key = existing_settings.secret_access_key;
        }
    }
    incoming
}

/// 在全局同步锁里跑一次操作。
pub async fn run_with_lock<T, Fut>(operation: Fut) -> Result<T, AppError>
where
    Fut: std::future::Future<Output = Result<T, AppError>>,
{
    s3_sync_service::run_with_sync_lock(operation).await
}

/// 下载 + 下载后的投影，整段持锁。
///
/// 与 WebDAV 同理：投影必须和下载在**同一次**持锁里做完，否则另一轮恢复可能
/// 在「库已应用、投影还没跑完」的窗口里插进来。
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
                crate::services::s3_auto_sync::AutoSyncSuppressionGuard::new();
            download.await?
        };
        project(result).await
    })
    .await
}
