//! 项目 Profile 管理命令

use tauri::{Emitter, Manager, State};

use crate::app_config::AppType;
use crate::services::profile::{ProfileScope, ProfileService};
use crate::store::AppState;

pub use crate::services::profile_view::{CurrentProfileIds, ProfileDto, ProfilesResponse};

/// Profile 应用完成后的统一收尾：发事件 + 重建托盘菜单
///
/// 只对项目所属分组内的应用发 provider-switched。UI 与托盘两个入口必须
/// 共用此函数，保证事件 payload 形状一致（前端 App.tsx 的
/// provider-switched 监听依赖该形状）。`desktop_was_mapping` 是应用前
/// Claude Desktop 是否在用模型映射卡，见 [`desktop_uses_mapping`]。
pub fn emit_profile_apply_events(
    app: &tauri::AppHandle,
    state: &AppState,
    profile_id: &str,
    scope: ProfileScope,
    desktop_was_mapping: bool,
) {
    // 事件 payload 的构造只有一份，在 `services::profile_view`：服务端经
    // event_sink 发、桌面经 AppHandle 发，形状因此不可能各走各的。
    for (name, payload) in crate::services::profile_view::apply_events(state, profile_id, scope) {
        if let Err(e) = app.emit(name, payload) {
            log::error!("发事件 {name} 失败: {e}");
        }
    }
    crate::tray::refresh_tray_menu(app);
    if scope.apps().contains(&AppType::ClaudeDesktop) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = app.try_state::<AppState>() {
                crate::mode::controller::sync_desktop_mapping_service(
                    state.inner(),
                    desktop_was_mapping,
                )
                .await;
            }
        });
    }
}

/// 应用项目前取一次：这个分组含 Claude Desktop 且它当前是模型映射卡。
pub fn desktop_uses_mapping(state: &AppState, scope: ProfileScope) -> bool {
    scope.apps().contains(&AppType::ClaudeDesktop)
        && crate::claude_desktop_config::current_provider_uses_proxy(&state.db)
}

#[tauri::command]
pub fn list_profiles(state: State<'_, AppState>) -> Result<ProfilesResponse, String> {
    let profiles = ProfileService::list(&state).map_err(|e| e.to_string())?;
    let current_ids = CurrentProfileIds {
        claude: state
            .db
            .get_current_profile_id(ProfileScope::Claude.as_str())
            .map_err(|e| e.to_string())?,
        claude_desktop: state
            .db
            .get_current_profile_id(ProfileScope::ClaudeDesktop.as_str())
            .map_err(|e| e.to_string())?,
        codex: state
            .db
            .get_current_profile_id(ProfileScope::Codex.as_str())
            .map_err(|e| e.to_string())?,
    };
    Ok(ProfilesResponse {
        profiles: profiles.into_iter().map(ProfileDto::from).collect(),
        current_ids,
    })
}

#[tauri::command]
pub fn create_profile(
    state: State<'_, AppState>,
    name: String,
    scope: String,
) -> Result<ProfileDto, String> {
    let scope = ProfileScope::parse(&scope).map_err(|e| e.to_string())?;
    ProfileService::create(&state, &name, scope)
        .map(ProfileDto::from)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_profile(
    state: State<'_, AppState>,
    id: String,
    name: Option<String>,
    resnapshot: Option<bool>,
    scope: Option<String>,
) -> Result<ProfileDto, String> {
    let scope = scope
        .map(|s| ProfileScope::parse(&s))
        .transpose()
        .map_err(|e| e.to_string())?;
    ProfileService::update(&state, &id, name, resnapshot.unwrap_or(false), scope)
        .map(ProfileDto::from)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_profile(state: State<'_, AppState>, id: String) -> Result<(), String> {
    ProfileService::delete(&state, &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_current_profile(state: State<'_, AppState>, scope: String) -> Result<(), String> {
    let scope = ProfileScope::parse(&scope).map_err(|e| e.to_string())?;
    state
        .db
        .set_current_profile_id(scope.as_str(), None)
        .map_err(|e| e.to_string())
}

/// 应用项目快照（只作用于发起页所属分组内的应用）。
///
/// 注意：必须保持同步命令（跑在 Tauri 线程池）——`ProviderService::switch`
/// 内部使用 block_on 获取切换锁，放进 async 命令会在运行时线程上 panic。
#[tauri::command]
pub fn apply_profile(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    scope: String,
) -> Result<Vec<String>, String> {
    let scope = ProfileScope::parse(&scope).map_err(|e| e.to_string())?;
    let desktop_was_mapping = desktop_uses_mapping(&state, scope);
    let warnings = ProfileService::apply(&state, &id, scope).map_err(|e| e.to_string())?;
    emit_profile_apply_events(&app, &state, &id, scope, desktop_was_mapping);
    Ok(warnings)
}
