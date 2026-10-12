//! `/api/invoke` 命令分发表。
//!
//! 桌面版的 302 个 `#[tauri::command]` 定义在 `commands/` 里，而那个模块只在
//! `desktop` feature 下编译。服务端因此**不复用命令函数**，而是把命令体里那层
//! 薄封装（解析参数 → 调 service 层）在这里重写一遍——业务逻辑仍然只有一份，
//! 在 `services/` 里。
//!
//! 目前实现的范围：启动链路、供应商页、设置保存、本地路由（代理）与故障转移队列、
//! 导入导出与数据库备份、会话浏览，以及 MCP / Skills / Prompts / 用量统计四页
//! （后四页各自成表，见 [`super::mcp`] 等同级模块）。
//! 未实现的命令返回 `E_NOT_IMPLEMENTED:<cmd>`，前端会以普通错误提示，不会白屏。
//! 凡是命令体里有真实逻辑的（如故障转移的开启流程），逻辑都放在 `services/` 里
//! 由两种构建共用——这里只做参数解析与调用。

use std::collections::HashMap;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use axum::extract::State as AxumState;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use super::Context;
use crate::app_config::AppType;
use crate::provider::Provider;
use crate::services::provider::{ProviderService, ProviderSortUpdate};

/// 分发表的函数指针类型。
pub type HandlerFuture = Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>>;
pub type Handler = fn(Arc<Context>, Value) -> HandlerFuture;

/// 前端发来的调用请求（与 Tauri 的 `invoke(cmd, args)` 一一对应）。
#[derive(serde::Deserialize)]
pub struct InvokeRequest {
    pub cmd: String,
    #[serde(default)]
    pub args: Value,
}

/// `POST /api/invoke`
pub async fn invoke(
    AxumState(context): AxumState<Arc<Context>>,
    Json(request): Json<InvokeRequest>,
) -> Response {
    // 数据库不可用（版本过新/初始化失败）时只放行 get_init_error，
    // 让前端渲染恢复界面；其余命令给出明确错误。
    if context.state.is_none() && request.cmd != "get_init_error" {
        return Json(json!({
            "ok": false,
            "error": "E_INIT: 数据库未就绪，请查看服务端日志",
        }))
        .into_response();
    }

    let Some(handler) = dispatch_table().get(request.cmd.as_str()).copied() else {
        return Json(json!({
            "ok": false,
            "error": format!("E_NOT_IMPLEMENTED: {}", request.cmd),
        }))
        .into_response();
    };

    match handler(context, request.args).await {
        Ok(data) => Json(json!({ "ok": true, "data": data })).into_response(),
        Err(error) => Json(json!({ "ok": false, "error": error })).into_response(),
    }
}

/// `GET /api/commands`：已实现的命令名（便于排查）。
pub async fn list_commands() -> Response {
    let mut names: Vec<&str> = dispatch_table().keys().copied().collect();
    names.sort_unstable();
    Json(json!({ "commands": names })).into_response()
}

/// `GET /api/capabilities`：这个服务端有哪些能力。
///
/// 前端据此**主动隐藏**不可用的入口，而不是让用户点进去看报错。键名按界面概念命名
/// （页面级 / 页面内动作），布尔值直接对应"能不能用"。
pub async fn capabilities() -> Response {
    let table = dispatch_table();
    let has = |cmd: &str| table.contains_key(cmd);

    let features = json!({
        // —— 页面级 ——
        "pageProviders": has("get_providers"),
        "pageRouting": has("start_proxy_server"),
        "pageSessions": has("list_sessions"),
        "pageImportExport": has("export_config_to_file"),
        "pageUsage": has("get_usage_summary"),
        "pageMcp": has("get_mcp_servers"),
        "pageSkills": has("get_installed_skills"),
        "pagePrompts": has("get_prompts"),
        "pageAuth": false,    // 托管账号（Copilot / Codex / xAI）登录流程未接入
        "pageApps": false,    // CLI 工具版本检测与安装未接入

        // —— 页面内动作 ——
        "sessionStream": has("web_session_transcript"),
        "sessionReveal": false,   // 服务端没有文件管理器
        "sessionTerminal": false, // 服务端没有桌面终端
        "pickDirectory": false,   // 浏览器无法为服务端选目录
        "openInFileManager": false, // 「在文件管理器里打开」类按钮

        // —— 桌面专属 ——
        "tray": false,
        "updater": false,
    });

    let mut commands: Vec<&str> = table.keys().copied().collect();
    commands.sort_unstable();

    Json(json!({
        "mode": "web",
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "features": features,
        "commands": commands,
    }))
    .into_response()
}

fn dispatch_table() -> &'static HashMap<&'static str, Handler> {
    static TABLE: OnceLock<HashMap<&'static str, Handler>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = HashMap::new();

        // ---- 启动 ----
        table.insert("get_init_error", get_init_error as Handler);
        table.insert("get_migration_result", get_migration_result as Handler);
        table.insert(
            "get_skills_migration_result",
            get_skills_migration_result as Handler,
        );
        table.insert(
            "take_startup_attach_failures",
            take_startup_attach_failures as Handler,
        );
        table.insert("take_tray_navigation", take_tray_navigation as Handler);
        table.insert("tray_app_page_seen", tray_app_page_seen as Handler);
        table.insert("check_env_conflicts", check_env_conflicts as Handler);

        // ---- 设置 ----
        table.insert("get_settings", get_settings as Handler);
        table.insert("save_settings", save_settings as Handler);
        table.insert("get_config_dir", get_config_dir as Handler);
        table.insert("open_config_folder", open_config_folder as Handler);
        table.insert("update_tray_menu", update_tray_menu as Handler);

        // ---- 供应商 ----
        table.insert("get_providers", get_providers as Handler);
        table.insert("get_current_provider", get_current_provider as Handler);
        table.insert("add_provider", add_provider as Handler);
        table.insert("update_provider", update_provider as Handler);
        table.insert("delete_provider", delete_provider as Handler);
        table.insert(
            "remove_provider_from_live_config",
            remove_provider_from_live_config as Handler,
        );
        table.insert("switch_provider", switch_provider as Handler);
        table.insert(
            "get_provider_editor_view",
            get_provider_editor_view as Handler,
        );
        table.insert("import_default_config", import_default_config as Handler);
        // 累加模式应用各自的 live 导入（桌面版是三条同名命令）
        table.insert(
            "import_opencode_providers_from_live",
            import_opencode_providers_from_live as Handler,
        );
        table.insert(
            "import_openclaw_providers_from_live",
            import_openclaw_providers_from_live as Handler,
        );
        table.insert(
            "import_hermes_providers_from_live",
            import_hermes_providers_from_live as Handler,
        );
        table.insert(
            "get_opencode_live_provider_ids",
            get_opencode_live_provider_ids as Handler,
        );
        table.insert(
            "update_providers_sort_order",
            update_providers_sort_order as Handler,
        );
        table.insert(
            "ensure_codex_official_provider",
            ensure_codex_official_provider as Handler,
        );
        table.insert(
            "ensure_grokbuild_official_provider",
            ensure_grokbuild_official_provider as Handler,
        );

        // ---- 代理状态 / 本地路由 ----
        table.insert("get_proxy_status", get_proxy_status as Handler);
        table.insert(
            "get_proxy_takeover_status",
            get_proxy_takeover_status as Handler,
        );
        table.insert("start_proxy_server", start_proxy_server as Handler);
        table.insert("stop_proxy_server", stop_proxy_server as Handler);
        table.insert(
            "stop_proxy_with_restore",
            stop_proxy_with_restore as Handler,
        );
        table.insert(
            "get_global_proxy_config",
            get_global_proxy_config as Handler,
        );
        table.insert(
            "update_global_proxy_config",
            update_global_proxy_config as Handler,
        );
        table.insert(
            "get_proxy_config_for_app",
            get_proxy_config_for_app as Handler,
        );
        table.insert(
            "update_proxy_config_for_app",
            update_proxy_config_for_app as Handler,
        );

        // ---- 模式（直连 / 路由）----
        table.insert("get_app_mode", get_app_mode as Handler);
        table.insert(
            "set_proxy_takeover_for_app",
            set_proxy_takeover_for_app as Handler,
        );
        table.insert("set_proxy_route", set_proxy_route as Handler);
        table.insert("get_direct_provider", get_direct_provider as Handler);

        // ---- 故障转移队列 ----
        table.insert("get_failover_queue", get_failover_queue as Handler);
        table.insert(
            "get_available_providers_for_failover",
            get_available_providers_for_failover as Handler,
        );
        table.insert("add_to_failover_queue", add_to_failover_queue as Handler);
        table.insert(
            "remove_from_failover_queue",
            remove_from_failover_queue as Handler,
        );
        table.insert(
            "get_auto_failover_enabled",
            get_auto_failover_enabled as Handler,
        );
        table.insert(
            "set_auto_failover_enabled",
            set_auto_failover_enabled as Handler,
        );

        // ---- 熔断 / 健康 ----
        table.insert(
            "get_circuit_breaker_config",
            get_circuit_breaker_config as Handler,
        );
        table.insert(
            "update_circuit_breaker_config",
            update_circuit_breaker_config as Handler,
        );
        table.insert(
            "get_circuit_breaker_stats",
            get_circuit_breaker_stats as Handler,
        );
        table.insert("reset_circuit_breaker", reset_circuit_breaker as Handler);
        table.insert("get_provider_health", get_provider_health as Handler);

        // ---- 自定义端点 / 测速 ----
        table.insert("get_custom_endpoints", get_custom_endpoints as Handler);
        table.insert("add_custom_endpoint", add_custom_endpoint as Handler);
        table.insert("remove_custom_endpoint", remove_custom_endpoint as Handler);
        table.insert(
            "update_endpoint_last_used",
            update_endpoint_last_used as Handler,
        );
        table.insert("test_api_endpoints", test_api_endpoints as Handler);

        // ---- 全局出站代理 ----
        table.insert("get_global_proxy_url", get_global_proxy_url as Handler);
        table.insert("set_global_proxy_url", set_global_proxy_url as Handler);
        table.insert("test_proxy_url", test_proxy_url as Handler);
        table.insert(
            "get_upstream_proxy_status",
            get_upstream_proxy_status as Handler,
        );
        table.insert("scan_local_proxies", scan_local_proxies as Handler);

        // ---- Stack（聚合）模式 ----
        table.insert(
            "exit_proxy_apps_in_mode",
            exit_proxy_apps_in_mode as Handler,
        );
        table.insert("get_proxy_stack", get_proxy_stack as Handler);
        table.insert("set_proxy_stack_member", set_proxy_stack_member as Handler);
        table.insert(
            "adopt_codex_stack_catalog",
            adopt_codex_stack_catalog as Handler,
        );
        table.insert(
            "restart_codex_app_server_daemon",
            restart_codex_app_server_daemon as Handler,
        );

        // ---- 连通性检查 / 拉模型 ----
        table.insert("stream_check_provider", stream_check_provider as Handler);
        table.insert(
            "fetch_models_for_config",
            fetch_models_for_config as Handler,
        );

        // ---- 设置（日志 / 整流 / 优化）----
        table.insert("get_log_config", get_log_config as Handler);
        table.insert("set_log_config", set_log_config as Handler);
        table.insert("get_rectifier_config", get_rectifier_config as Handler);
        table.insert("set_rectifier_config", set_rectifier_config as Handler);
        table.insert("get_optimizer_config", get_optimizer_config as Handler);
        table.insert("set_optimizer_config", set_optimizer_config as Handler);

        // ---- 环境变量 ----
        table.insert("delete_env_vars", delete_env_vars as Handler);
        table.insert("restore_env_backup", restore_env_backup as Handler);

        // ---- 备份位置 ----
        table.insert("list_backup_locations", list_backup_locations as Handler);
        table.insert("delete_backup_location", delete_backup_location as Handler);

        // ---- Profile（配置集）----
        table.insert("list_profiles", list_profiles as Handler);
        table.insert("create_profile", create_profile as Handler);
        table.insert("update_profile", update_profile as Handler);
        table.insert("delete_profile", delete_profile as Handler);
        table.insert("apply_profile", apply_profile as Handler);
        table.insert("clear_current_profile", clear_current_profile as Handler);

        // ---- WebDAV 同步 ----
        table.insert("webdav_test_connection", webdav_test_connection as Handler);
        table.insert("webdav_sync_upload", webdav_sync_upload as Handler);
        table.insert("webdav_sync_download", webdav_sync_download as Handler);
        table.insert(
            "webdav_sync_save_settings",
            webdav_sync_save_settings as Handler,
        );
        table.insert(
            "webdav_sync_fetch_remote_info",
            webdav_sync_fetch_remote_info as Handler,
        );

        // ---- Codex 会话压缩 / 过期提示 / 日志 ----
        table.insert(
            "get_codex_session_compression",
            get_codex_session_compression as Handler,
        );
        table.insert(
            "set_codex_session_compression",
            set_codex_session_compression as Handler,
        );
        table.insert(
            "get_codex_sessions_disk_usage",
            get_codex_sessions_disk_usage as Handler,
        );
        table.insert(
            "acknowledge_codex_stale_clients",
            acknowledge_codex_stale_clients as Handler,
        );
        table.insert("reveal_app_log_file", reveal_app_log_file as Handler);

        // ---- Pi 状态 / 用量脚本 / 用量首日 ----
        table.insert("get_pi_current_state", get_pi_current_state as Handler);
        table.insert(
            "update_pi_provider_usage_script",
            update_pi_provider_usage_script as Handler,
        );
        table.insert(
            "get_pi_session_discovery",
            get_pi_session_discovery as Handler,
        );
        table.insert("get_usage_first_date", get_usage_first_date as Handler);

        // ---- S3 同步 ----
        table.insert("s3_test_connection", s3_test_connection as Handler);
        table.insert("s3_sync_upload", s3_sync_upload as Handler);
        table.insert("s3_sync_download", s3_sync_download as Handler);
        table.insert("s3_sync_save_settings", s3_sync_save_settings as Handler);
        table.insert(
            "s3_sync_fetch_remote_info",
            s3_sync_fetch_remote_info as Handler,
        );

        // ---- 用量额度：余额 / 编码套餐 / 订阅 ----
        table.insert("get_subscription_quota", get_subscription_quota as Handler);
        table.insert("get_balance", get_balance as Handler);
        table.insert("get_coding_plan_quota", get_coding_plan_quota as Handler);

        // ---- 导入导出与数据库备份 ----
        table.insert("export_config_to_file", export_config_to_file as Handler);
        table.insert(
            "import_config_from_file",
            import_config_from_file as Handler,
        );
        table.insert("create_db_backup", create_db_backup as Handler);
        table.insert("list_db_backups", list_db_backups as Handler);
        table.insert("restore_db_backup", restore_db_backup as Handler);
        table.insert("rename_db_backup", rename_db_backup as Handler);
        table.insert("delete_db_backup", delete_db_backup as Handler);
        // 浏览器里没有"保存文件对话框"：由前端 shim 调它拿一个服务端导出路径
        table.insert(
            "web_allocate_export_path",
            web_allocate_export_path as Handler,
        );

        // ---- 会话浏览 ----
        table.insert("list_sessions", list_sessions as Handler);
        table.insert("get_session_messages", get_session_messages as Handler);
        table.insert(
            "get_session_block_content",
            get_session_block_content as Handler,
        );
        table.insert("delete_session", delete_session as Handler);
        table.insert("delete_sessions", delete_sessions as Handler);
        table.insert(
            "export_session_markdown",
            export_session_markdown as Handler,
        );
        // 浏览器里没有 Channel：shim 用它读整段会话再分块喂回调
        table.insert("web_session_transcript", web_session_transcript as Handler);
        table.insert("reveal_session_path", reveal_session_path as Handler);
        table.insert(
            "launch_session_terminal",
            launch_session_terminal as Handler,
        );

        // ---- MCP / Prompts / Skills / 用量 ----
        // 这四页命令量大且各自独立，分表放在子模块里，这里只做注册。
        for (name, handler) in super::mcp::HANDLERS {
            table.insert(name, *handler);
        }
        for (name, handler) in super::prompts::HANDLERS {
            table.insert(name, *handler);
        }
        for (name, handler) in super::skills::HANDLERS {
            table.insert(name, *handler);
        }
        for (name, handler) in super::usage::HANDLERS {
            table.insert(name, *handler);
        }

        table
    })
}

// ============================================================================
// 工具函数
// ============================================================================

pub fn serializable<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

pub fn parse<T: DeserializeOwned>(raw: Value) -> Result<T, String> {
    serde_json::from_value(raw).map_err(|error| format!("参数解析失败: {error}"))
}

/// 在阻塞线程池里跑同步的 service 调用（SQLite、文件读写都在其中）。
pub async fn blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    crate::host::spawn_blocking(work)
        .await
        .map_err(|error| format!("后台任务异常退出: {error}"))?
}

/// 把一个同步调用包成 handler，并丢进阻塞线程池：SQLite 聚合、文件读写用它。
pub fn deferred<T, F>(work: F) -> HandlerFuture
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Serialize + Send + 'static,
{
    Box::pin(async move { blocking(work).await.and_then(serializable) })
}

pub fn to_app_type(app: &str) -> Result<AppType, String> {
    AppType::from_str(app).map_err(|error| error.to_string())
}

// ============================================================================
// 启动
// ============================================================================

fn get_init_error(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { serializable(crate::init_status::get_init_error()) })
}

fn get_migration_result(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { serializable(crate::init_status::take_migration_success()) })
}

fn get_skills_migration_result(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { serializable(crate::init_status::take_skills_migration_result()) })
}

fn take_startup_attach_failures(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { serializable(crate::mode::controller::take_startup_attach_failures()) })
}

/// 托盘导航在 web 模式没有对应物。
fn take_tray_navigation(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { Ok(Value::Null) })
}

fn tray_app_page_seen(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { Ok(Value::Null) })
}

fn check_env_conflicts(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            app: String,
        }
        let Args { app } = parse(args)?;
        serializable(crate::services::env_checker::check_env_conflicts(&app)?)
    })
}

// ============================================================================
// 设置
// ============================================================================

fn get_settings(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { serializable(crate::settings::get_settings_for_frontend()) })
}

fn save_settings(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            settings: crate::settings::AppSettings,
        }
        let Args { settings } = parse(args)?;
        let state = context.require_state()?;

        blocking(move || {
            let existing = crate::settings::get_settings();
            let merged = crate::settings::merge_settings_for_save(settings, &existing);
            let unify_changed =
                merged.unify_codex_session_history != existing.unify_codex_session_history;

            crate::settings::update_settings(merged).map_err(|error| error.to_string())?;

            // 统一 Codex 会话开关变更时立刻重写当前官方供应商的 live 配置
            if unify_changed {
                if let Err(error) =
                    crate::services::provider::reapply_current_codex_official_live(&state)
                {
                    log::warn!("统一 Codex 会话历史开关变更后重写 live 配置失败: {error}");
                }
            }

            Ok(Value::Bool(true))
        })
        .await
    })
}

fn get_config_dir(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            app: String,
        }
        let Args { app } = parse(args)?;
        let dir = match to_app_type(&app)? {
            AppType::Claude => crate::config::get_claude_config_dir(),
            AppType::ClaudeDesktop => crate::claude_desktop_config::get_config_library_path()
                .map_err(|e| e.to_string())?,
            AppType::Codex => crate::codex_config::get_codex_config_dir(),
            AppType::Gemini => crate::gemini_config::get_gemini_dir(),
            AppType::GrokBuild => crate::grok_config::get_grok_config_dir(),
            AppType::OpenCode => crate::opencode_config::get_opencode_dir(),
            AppType::OpenClaw => crate::openclaw_config::get_openclaw_dir(),
            AppType::Hermes => crate::hermes_config::get_hermes_dir(),
            AppType::Pi => crate::pi_config::get_pi_agent_dir().map_err(|e| e.to_string())?,
            AppType::Mcode => crate::mcode_config::config_path()
                .parent()
                .map(|path| path.to_path_buf())
                .ok_or_else(|| "无法解析 MiniMax Code 配置目录".to_string())?,
        };
        Ok(Value::String(dir.to_string_lossy().to_string()))
    })
}

/// 服务端没有桌面会话，打不开「配置目录」窗口。
fn open_config_folder(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { Err("Web 模式下请在服务器上直接打开该目录".to_string()) })
}

/// 没有托盘；前端拿 `true` 当作「刷新成功」即可。
fn update_tray_menu(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { Ok(Value::Bool(true)) })
}

// ============================================================================
// 供应商
// ============================================================================

fn get_providers(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            app: String,
        }
        let Args { app } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            let providers = ProviderService::list(&state, app_type).map_err(|e| e.to_string())?;
            serializable(providers)
        })
        .await
    })
}

fn get_current_provider(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            app: String,
        }
        let Args { app } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            let current = ProviderService::current(&state, app_type).map_err(|e| e.to_string())?;
            serializable(current)
        })
        .await
    })
}

fn add_provider(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app: String,
            provider: Provider,
            add_to_live: Option<bool>,
            editor_save: Option<crate::services::provider::EditorSave>,
        }
        let Args {
            app,
            provider,
            add_to_live,
            editor_save,
        } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            let added = ProviderService::add_from_editor(
                &state,
                app_type,
                provider,
                add_to_live.unwrap_or(true),
                editor_save,
            )
            .map_err(|e| e.to_string())?;
            Ok(Value::Bool(added))
        })
        .await
    })
}

fn update_provider(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app: String,
            provider: Provider,
            original_id: Option<String>,
            editor_save: Option<crate::services::provider::EditorSave>,
        }
        let Args {
            app,
            provider,
            original_id,
            editor_save,
        } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            let updated = ProviderService::update_from_editor(
                &state,
                app_type,
                original_id.as_deref(),
                provider,
                editor_save,
            )
            .map_err(|e| e.to_string())?;
            Ok(Value::Bool(updated))
        })
        .await
    })
}

fn delete_provider(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            app: String,
            id: String,
        }
        let Args { app, id } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            ProviderService::delete(&state, app_type, &id).map_err(|e| e.to_string())?;
            Ok(Value::Bool(true))
        })
        .await
    })
}

fn remove_provider_from_live_config(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            app: String,
            id: String,
        }
        let Args { app, id } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            ProviderService::remove_from_live_config(&state, app_type, &id)
                .map_err(|e| e.to_string())?;
            Ok(Value::Bool(true))
        })
        .await
    })
}

fn switch_provider(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            app: String,
            id: String,
        }
        let Args { app, id } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            let result =
                ProviderService::switch(&state, app_type, &id).map_err(|e| e.to_string())?;
            serializable(result)
        })
        .await
    })
}

fn get_provider_editor_view(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app: String,
            settings_config: Value,
            category: Option<String>,
            provider_id: Option<String>,
        }
        let Args {
            app,
            settings_config,
            category,
            provider_id,
        } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            let category = ProviderService::editor_category(
                &state,
                &app_type,
                provider_id.as_deref(),
                category,
            )
            .map_err(|e| e.to_string())?;
            let view = ProviderService::editor_view(
                &state,
                app_type,
                &settings_config,
                category.as_deref(),
            )
            .map_err(|e| e.to_string())?;
            serializable(view)
        })
        .await
    })
}

fn import_default_config(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            app: String,
        }
        let Args { app } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            let imported = ProviderService::import_default_config(&state, app_type)
                .map_err(|e| e.to_string())?;
            Ok(Value::Bool(imported))
        })
        .await
    })
}

/// 累加模式应用（OpenCode / OpenClaw / Hermes）的「从 live 导入供应商」。
///
/// 通用导入 `import_default_config` 对这几个应用直接返回 false，它们各自有专门的
/// 同步函数——桌面版是三条同名命令，这里把同一批 service 函数接上，逻辑不重复。
fn import_additive_providers(
    context: Arc<Context>,
    import: fn(&crate::store::AppState) -> Result<usize, crate::error::AppError>,
) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        blocking(move || {
            import(&state)
                .map(|count| json!(count))
                .map_err(|error| error.to_string())
        })
        .await
    })
}

fn import_opencode_providers_from_live(context: Arc<Context>, _args: Value) -> HandlerFuture {
    import_additive_providers(
        context,
        crate::services::provider::import_opencode_providers_from_live,
    )
}

fn import_openclaw_providers_from_live(context: Arc<Context>, _args: Value) -> HandlerFuture {
    import_additive_providers(
        context,
        crate::services::provider::import_openclaw_providers_from_live,
    )
}

fn import_hermes_providers_from_live(context: Arc<Context>, _args: Value) -> HandlerFuture {
    import_additive_providers(
        context,
        crate::services::provider::import_hermes_providers_from_live,
    )
}

/// OpenCode 页用它判断“某个供应商是否已经在 opencode.json 里”。
fn get_opencode_live_provider_ids(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    deferred(|| {
        crate::opencode_config::get_providers()
            .map(|providers| providers.keys().cloned().collect::<Vec<String>>())
            .map_err(|error| error.to_string())
    })
}

fn update_providers_sort_order(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            app: String,
            updates: Vec<ProviderSortUpdate>,
        }
        let Args { app, updates } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            let changed = ProviderService::update_sort_order(&state, app_type, updates)
                .map_err(|e| e.to_string())?;
            Ok(Value::Bool(changed))
        })
        .await
    })
}

fn ensure_codex_official_provider(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        blocking(move || {
            let ensured = state
                .db
                .ensure_official_seed_by_id(
                    crate::database::CODEX_OFFICIAL_PROVIDER_ID,
                    AppType::Codex,
                )
                .map_err(|e| e.to_string())?;
            Ok(Value::Bool(ensured))
        })
        .await
    })
}

fn ensure_grokbuild_official_provider(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        blocking(move || {
            let ensured = state
                .db
                .ensure_official_seed_by_id(
                    crate::database::GROKBUILD_OFFICIAL_PROVIDER_ID,
                    AppType::GrokBuild,
                )
                .map_err(|e| e.to_string())?;
            Ok(Value::Bool(ensured))
        })
        .await
    })
}

// ============================================================================
// 代理状态（服务端不跑本地代理，但界面需要能渲染）
// ============================================================================

fn get_proxy_status(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let status = state.proxy_service.get_status().await?;
        serializable(status)
    })
}

fn get_proxy_takeover_status(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let status = state.proxy_service.get_takeover_status().await?;
        serializable(status)
    })
}

// ============================================================================
// 本地路由（代理）
// ============================================================================

/// 只有支持本地路由的应用才有代理配置与模式。
fn require_proxy_app(app_type: &str) -> Result<AppType, String> {
    let app = AppType::from_str(app_type).map_err(|error| format!("无效的应用类型: {error}"))?;
    if !app.supports_local_proxy() {
        return Err(format!("{} 不支持本地路由", app.as_str()));
    }
    Ok(app)
}

fn start_proxy_server(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let info = state.proxy_service.start().await?;
        serializable(info)
    })
}

fn stop_proxy_server(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        // 与桌面版一致：还有应用处于接管状态时先不停止，避免 CLI 指向一个已关闭的端口
        let takeover = state.proxy_service.get_takeover_status().await?;
        if takeover.claude
            || takeover.codex
            || takeover.gemini
            || takeover.grokbuild
            || takeover.opencode
            || takeover.openclaw
        {
            return Err(
                "仍有应用处于代理接管状态，请先在设置中关闭对应应用接管后再停止本地路由。"
                    .to_string(),
            );
        }
        state.proxy_service.stop().await?;
        Ok(Value::Bool(true))
    })
}

/// 关闭本地路由：所有应用退回直连，再停止代理服务器。
fn stop_proxy_with_restore(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        crate::mode::controller::exit_all(&state).await?;
        Ok(Value::Bool(true))
    })
}

fn get_global_proxy_config(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let config = state
            .db
            .get_global_proxy_config()
            .await
            .map_err(|e| e.to_string())?;
        serializable(config)
    })
}

fn update_global_proxy_config(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            config: crate::proxy::types::GlobalProxyConfig,
        }
        let Args { config } = parse(args)?;
        let state = context.require_state()?;

        // 地址/端口变了就重启服务，并按新地址重写接上路由的客户端
        let restarted = state.proxy_service.update_global_config(&config).await?;
        if restarted {
            crate::mode::controller::resync_routes(&state).await?;
        }
        Ok(Value::Bool(true))
    })
}

fn get_proxy_config_for_app(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
        }
        let Args { app_type } = parse(args)?;
        require_proxy_app(&app_type)?;

        let state = context.require_state()?;
        let config = state
            .db
            .get_proxy_config_for_app(&app_type)
            .await
            .map_err(|e| e.to_string())?;
        serializable(config)
    })
}

fn update_proxy_config_for_app(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            config: crate::proxy::types::AppProxyConfig,
        }
        let Args { config } = parse(args)?;
        let state = context.require_state()?;

        let app_type = config.app_type.clone();
        let app = require_proxy_app(&app_type)?;
        let circuit_config = crate::proxy::CircuitBreakerConfig::from(&config);

        let mut config = config;
        // `enabled` 是模式的镜像，只由进入 / 退出代理改写
        config.enabled = crate::mode::current::is_proxy(&app);

        state
            .db
            .update_proxy_config_for_app(config)
            .await
            .map_err(|e| e.to_string())?;

        state
            .proxy_service
            .update_circuit_breaker_config_for_app(&app_type, circuit_config)
            .await?;
        Ok(Value::Bool(true))
    })
}

// ============================================================================
// 模式（直连 / 路由）
// ============================================================================

fn get_app_mode(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
        }
        let Args { app_type } = parse(args)?;
        let app = require_proxy_app(&app_type)?;
        let state = context.require_state()?;
        blocking(move || {
            let view =
                crate::mode::controller::app_mode_view(&state, &app).map_err(|e| e.to_string())?;
            serializable(view)
        })
        .await
    })
}

fn set_proxy_takeover_for_app(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
            enabled: bool,
            stack: Option<bool>,
            route: Option<String>,
        }
        let Args {
            app_type,
            enabled,
            stack,
            route,
        } = parse(args)?;
        let app = require_proxy_app(&app_type)?;
        let state = context.require_state()?;

        if enabled {
            crate::mode::controller::enter_with_route(
                &state,
                &app,
                stack.unwrap_or(false),
                route.as_deref(),
            )
            .await?;
        } else {
            crate::mode::controller::exit(&state, &app).await?;
        }
        Ok(Value::Bool(true))
    })
}

fn set_proxy_route(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
            provider_id: String,
        }
        let Args {
            app_type,
            provider_id,
        } = parse(args)?;
        let app = require_proxy_app(&app_type)?;
        let state = context.require_state()?;

        crate::mode::controller::set_route(&state, &app, &provider_id).await?;
        Ok(Value::Bool(true))
    })
}

fn get_direct_provider(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
        }
        let Args { app_type } = parse(args)?;
        let app = require_proxy_app(&app_type)?;
        let state = context.require_state()?;
        blocking(move || {
            let provider_id = crate::mode::controller::direct_provider_id(&state, &app)
                .map_err(|e| e.to_string())?;
            serializable(provider_id)
        })
        .await
    })
}

// ============================================================================
// 故障转移队列（与桌面命令共用 `services::failover`）
// ============================================================================

fn get_failover_queue(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
        }
        let Args { app_type } = parse(args)?;
        let state = context.require_state()?;
        let queue = crate::services::failover::get_queue(&state, &app_type).await?;
        serializable(queue)
    })
}

fn get_available_providers_for_failover(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
        }
        let Args { app_type } = parse(args)?;
        let state = context.require_state()?;
        let providers =
            crate::services::failover::get_available_providers(&state, &app_type).await?;
        serializable(providers)
    })
}

fn add_to_failover_queue(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
            provider_id: String,
        }
        let Args {
            app_type,
            provider_id,
        } = parse(args)?;
        let state = context.require_state()?;
        crate::services::failover::add(&state, &app_type, &provider_id).await?;
        Ok(Value::Bool(true))
    })
}

fn remove_from_failover_queue(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
            provider_id: String,
        }
        let Args {
            app_type,
            provider_id,
        } = parse(args)?;
        let state = context.require_state()?;
        crate::services::failover::remove(&state, &app_type, &provider_id).await?;
        Ok(Value::Bool(true))
    })
}

fn get_auto_failover_enabled(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
        }
        let Args { app_type } = parse(args)?;
        let state = context.require_state()?;
        let enabled = crate::services::failover::auto_failover_enabled(&state, &app_type).await?;
        serializable(enabled)
    })
}

fn set_auto_failover_enabled(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
            enabled: bool,
        }
        let Args { app_type, enabled } = parse(args)?;
        let state = context.require_state()?;

        let p1_provider_id =
            crate::services::failover::set_auto_failover_enabled(&state, &app_type, enabled)
                .await?;

        if let Some(provider_id) = p1_provider_id {
            // 与桌面命令同名同 payload：前端据此刷新当前供应商
            crate::event_sink::emit(
                "provider-switched",
                json!({
                    "appType": app_type,
                    "providerId": provider_id,
                    "source": "failoverEnabled"
                }),
            );
        }
        Ok(Value::Bool(true))
    })
}

// ============================================================================
// 熔断 / 健康
//
// 桌面版这五条住在 commands/proxy.rs，而那个模块只在 desktop feature 下编译。
// 其中四条的命令体只有「解析参数 → 调 db / proxy_service」这一层，所以照搬；
// 唯 `reset_circuit_breaker` 带真实分支（恢复后要不要按队列优先级回切），
// 逻辑已搬进 `services::failover`，两种构建共用同一份。
// ============================================================================

fn get_circuit_breaker_config(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let config = state
            .db
            .get_circuit_breaker_config()
            .await
            .map_err(|error| error.to_string())?;
        serializable(config)
    })
}

fn update_circuit_breaker_config(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            config: crate::proxy::CircuitBreakerConfig,
        }
        let Args { config } = parse(args)?;
        let state = context.require_state()?;

        // 顺序与桌面一致：先落库，再热更运行中的代理。库写失败时不该动运行态。
        state
            .db
            .update_circuit_breaker_config(&config)
            .await
            .map_err(|error| error.to_string())?;
        state
            .proxy_service
            .update_circuit_breaker_configs(config)
            .await?;
        Ok(Value::Bool(true))
    })
}

/// 上游本身就是个桩，这里照它的语义返回 `null`。
///
/// `commands/proxy.rs` 里写着「这个功能需要访问运行中的代理服务器的内存状态，
/// 目前先返回 None」。返回 `null` 而不是 `E_NOT_IMPLEMENTED`，是因为界面读的是
/// 「有没有统计」——`null` 会让它安静地不显示那一栏，与桌面版一致；
/// 报错反而会把它变成一个显眼的失败。
fn get_circuit_breaker_stats(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            #[allow(dead_code)]
            provider_id: String,
            app_type: String,
        }
        let Args { app_type, .. } = parse(args)?;
        // 仍然校验应用类型：无效的 app 该和桌面版一样被拒绝，桩不等于不校验
        require_proxy_app(&app_type)?;
        let _ = context;
        Ok(Value::Null)
    })
}

fn reset_circuit_breaker(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            provider_id: String,
            app_type: String,
        }
        let Args {
            provider_id,
            app_type,
        } = parse(args)?;
        let state = context.require_state()?;
        crate::services::failover::reset_circuit_breaker(&state, &provider_id, &app_type).await?;
        Ok(Value::Bool(true))
    })
}

fn get_provider_health(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            provider_id: String,
            app_type: String,
        }
        let Args {
            provider_id,
            app_type,
        } = parse(args)?;
        require_proxy_app(&app_type)?;
        let state = context.require_state()?;
        let health = state
            .db
            .get_provider_health(&provider_id, &app_type)
            .await
            .map_err(|error| error.to_string())?;
        serializable(health)
    })
}

// ============================================================================
// 自定义端点 / 测速
//
// 与熔断那组同理：桌面命令体只是「解析 → 调 `ProviderService` /
// `SpeedtestService`」，两个 service 都在非门控模块里，这里照搬即可。
// 命令层的参数是 `#[allow(non_snake_case)] providerId`（Tauri 直接收驼峰），
// 所以下面用 `rename_all = "camelCase"` 收同一份线上形状。
// ============================================================================

fn get_custom_endpoints(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app: String,
            provider_id: String,
        }
        let Args { app, provider_id } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            ProviderService::get_custom_endpoints(&state, app_type, &provider_id)
                .map_err(|error| error.to_string())
        })
        .await
        .and_then(serializable)
    })
}

fn add_custom_endpoint(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app: String,
            provider_id: String,
            url: String,
        }
        let Args {
            app,
            provider_id,
            url,
        } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            ProviderService::add_custom_endpoint(&state, app_type, &provider_id, url)
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(Value::Bool(true))
    })
}

fn remove_custom_endpoint(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app: String,
            provider_id: String,
            url: String,
        }
        let Args {
            app,
            provider_id,
            url,
        } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            ProviderService::remove_custom_endpoint(&state, app_type, &provider_id, url)
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(Value::Bool(true))
    })
}

fn update_endpoint_last_used(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app: String,
            provider_id: String,
            url: String,
        }
        let Args {
            app,
            provider_id,
            url,
        } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let app_type = to_app_type(&app)?;
            ProviderService::update_endpoint_last_used(&state, app_type, &provider_id, url)
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(Value::Bool(true))
    })
}

/// 测速不需要 state，也就不该去 `require_state()`：桌面版同样收不到 AppState。
fn test_api_endpoints(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            urls: Vec<String>,
            timeout_secs: Option<u64>,
        }
        let Args { urls, timeout_secs } = parse(args)?;
        let results =
            crate::services::speedtest::SpeedtestService::test_endpoints(urls, timeout_secs)
                .await
                .map_err(|error| error.to_string())?;
        serializable(results)
    })
}

// ============================================================================
// 全局出站代理
//
// 逻辑在 `services::global_proxy`（原本整段住在 desktop 门控的命令层里），
// 这里只做解析与调用。`test_proxy_url` 与 `upstream_proxy_status` 在桌面版
// 也不接 state，照此处理。
// ============================================================================

fn get_global_proxy_url(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let url = crate::services::global_proxy::get_global_proxy_url(&state)?;
        serializable(url)
    })
}

fn set_global_proxy_url(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            url: String,
        }
        let Args { url } = parse(args)?;
        let state = context.require_state()?;
        crate::services::global_proxy::set_global_proxy_url(&state, url)?;
        Ok(Value::Bool(true))
    })
}

fn test_proxy_url(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            url: String,
        }
        let Args { url } = parse(args)?;
        let result = crate::services::global_proxy::test_proxy_url(url).await?;
        serializable(result)
    })
}

fn get_upstream_proxy_status(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move { serializable(crate::services::global_proxy::upstream_proxy_status()) })
}

fn scan_local_proxies(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    // 纯 TCP 探测，丢进阻塞线程池，别占着 async runtime：`deferred` 就是
    // 「阻塞线程池 + 序列化」这条组合，不必手写 `blocking(...).await`
    deferred(|| Ok(crate::services::global_proxy::scan_local_proxies_blocking()))
}

// ============================================================================
// Stack（聚合）模式
//
// 与前面几组同理：命令体是「解析 → 调 `mode::controller`」，那个模块不在 desktop
// 门控里。唯一差别是桌面版还会刷托盘，而 web 模式没有托盘——`update_tray_menu`
// 在这里就是回 `true` 充当「刷新成功」，下面照同一约定略过刷新。
// ============================================================================

fn exit_proxy_apps_in_mode(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            stack: bool,
        }
        let Args { stack } = parse(args)?;
        let state = context.require_state()?;
        let exited = crate::mode::controller::exit_apps_in_mode(&state, stack).await?;
        serializable(exited)
    })
}

fn get_proxy_stack(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
        }
        let Args { app_type } = parse(args)?;
        let app = require_proxy_app(&app_type)?;
        let state = context.require_state()?;
        let view = crate::mode::controller::stack_view_with_clients(&state, &app).await?;
        serializable(view)
    })
}

fn set_proxy_stack_member(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
            provider_id: String,
            enabled: bool,
        }
        let Args {
            app_type,
            provider_id,
            enabled,
        } = parse(args)?;
        let app = require_proxy_app(&app_type)?;
        let state = context.require_state()?;
        // 桌面版这里还 `refresh_tray_menu(&app_handle)`：托盘的聚合子菜单只列名单
        // 成员，改完必须重建。web 模式没有托盘，也就没有那张子菜单要重建。
        let hint = crate::mode::controller::set_stack_member(&state, &app, &provider_id, enabled)
            .await
            .map_err(|error| format!("{error:?}"))?;
        serializable(hint)
    })
}

fn adopt_codex_stack_catalog(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let hint = crate::mode::controller::adopt_codex_stack_catalog(&state).await?;
        serializable(hint)
    })
}

/// 重启 Codex 的托管守护进程。
///
/// 桌面版命令体就这么点：不接 state，只把 `restart_daemon` 送进 runtime 隔离层
/// （它会写文件，不该在异步任务里做）。
fn restart_codex_app_server_daemon(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let outcome = crate::services::provider::codex_direct::off_runtime(
            crate::services::provider::codex_client_catalog::restart_daemon,
        )
        .await
        .map_err(|error| error.to_string())?;
        serializable(outcome)
    })
}

// ============================================================================
// 供应商连通性检查 / 拉取模型列表
// ============================================================================

/// 只探测 base_url 可达性，不发真实模型请求，也不碰熔断器。
///
/// 桌面版还接一个 `CopilotAuthState`，把 Copilot 供应商的 base_url 解析成当前
/// OAuth 端点。服务端没接入托管账号登录（`GET /api/capabilities` 的 `pageAuth`
/// 就是 false），因此这里恒传 `None`：非 Copilot 供应商本来就走 `None`，Copilot
/// 那一家退化为按 `settings_config` 里的地址探测——与「托管账号未接入」是一致的
/// 降级，而不是假装解析成功。
fn stream_check_provider(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: String,
            provider_id: String,
        }
        let Args {
            app_type,
            provider_id,
        } = parse(args)?;
        let app_type = to_app_type(&app_type)?;
        let state = context.require_state()?;

        let providers = state
            .db
            .get_all_providers(app_type.as_str())
            .map_err(|error| error.to_string())?;
        let provider = providers
            .get(&provider_id)
            .ok_or_else(|| format!("供应商 {provider_id} 不存在"))?;

        let result = crate::services::stream_check::StreamCheckService::check_with_retry(
            &app_type,
            provider,
            &crate::services::stream_check::StreamCheckConfig::default(),
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        serializable(result)
    })
}

/// 用 OpenAI 兼容的 `GET /v1/models` 拉模型列表，不需要 state。
fn fetch_models_for_config(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            base_url: String,
            api_key: String,
            is_full_url: Option<bool>,
            models_url: Option<String>,
            custom_user_agent: Option<String>,
            api_format: Option<String>,
            request_headers: Option<std::collections::BTreeMap<String, String>>,
        }
        let Args {
            base_url,
            api_key,
            is_full_url,
            models_url,
            custom_user_agent,
            api_format,
            request_headers,
        } = parse(args)?;

        // 与桌面版共用同一条 UA 归一：非法 UA 静默忽略，不阻断取模型
        let user_agent = crate::provider::parse_custom_user_agent(custom_user_agent.as_deref())
            .ok()
            .flatten();

        let models = crate::services::model_fetch::fetch_models(
            &base_url,
            &api_key,
            is_full_url.unwrap_or(false),
            models_url.as_deref(),
            user_agent,
            api_format.as_deref(),
            request_headers.as_ref(),
        )
        .await
        .map_err(|error| error.to_string())?;
        serializable(models)
    })
}

// ============================================================================
// 设置（日志 / 整流 / 优化）与环境变量
//
// 前六条的命令体就是「解析 → 调 `db`」，这里照搬。`set_log_config` 多一步
// `log::set_max_level`：桌面与服务端都要把新级别当场生效，否则写了库而运行时
// 还按旧级别过滤。两步、无分支，没有可抽出的「真实逻辑」，故镜像而非搬运——
// 若将来这一步长出分支，就该按本文件的约定移进 `services/`。
//
// 后两条走 `services::env_manager`（桌面命令本身就是一跳委托）。
// ============================================================================

fn get_log_config(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let config = state
            .db
            .get_log_config()
            .map_err(|error| error.to_string())?;
        serializable(config)
    })
}

fn set_log_config(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            config: crate::proxy::types::LogConfig,
        }
        let Args { config } = parse(args)?;
        let state = context.require_state()?;
        state
            .db
            .set_log_config(&config)
            .map_err(|error| error.to_string())?;
        // 与桌面命令同一步：写完库立刻让运行时按新级别过滤
        log::set_max_level(config.to_level_filter());
        Ok(Value::Bool(true))
    })
}

fn get_rectifier_config(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let config = state
            .db
            .get_rectifier_config()
            .map_err(|error| error.to_string())?;
        serializable(config)
    })
}

fn set_rectifier_config(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            config: crate::proxy::types::RectifierConfig,
        }
        let Args { config } = parse(args)?;
        let state = context.require_state()?;
        state
            .db
            .set_rectifier_config(&config)
            .map_err(|error| error.to_string())?;
        Ok(Value::Bool(true))
    })
}

fn get_optimizer_config(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let config = state
            .db
            .get_optimizer_config()
            .map_err(|error| error.to_string())?;
        serializable(config)
    })
}

fn set_optimizer_config(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            config: crate::proxy::types::OptimizerConfig,
        }
        let Args { config } = parse(args)?;
        let state = context.require_state()?;
        state
            .db
            .set_optimizer_config(&config)
            .map_err(|error| error.to_string())?;
        Ok(Value::Bool(true))
    })
}

/// 删掉冲突的环境变量，删前自动备份；返回备份信息。
fn delete_env_vars(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            conflicts: Vec<crate::services::env_checker::EnvConflict>,
        }
        let Args { conflicts } = parse(args)?;
        let backup = crate::services::env_manager::delete_env_vars(conflicts)?;
        serializable(backup)
    })
}

/// 从备份文件恢复环境变量。
fn restore_env_backup(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            backup_path: String,
        }
        let Args { backup_path } = parse(args)?;
        crate::services::env_manager::restore_from_backup(backup_path)?;
        Ok(Value::Bool(true))
    })
}

// ============================================================================
// Pi（agent）状态 / 用量脚本 / 用量首日
//
// 命令体都是一跳委托到 `services::pi_state`、`ProviderService` 与 db，原样搬。
// `get_usage_first_date` 的桌面版用 `run_db_query` 包一层阻塞查询，这里直接用
// 本文件的 `blocking` 做同一件事。
// ============================================================================

fn get_pi_current_state(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let current = crate::services::pi_state::PiStateService::current(&state)
            .map_err(|e| e.to_string())?;
        serializable(current)
    })
}

fn update_pi_provider_usage_script(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            id: String,
            usage_script: crate::provider::UsageScript,
        }
        let Args { id, usage_script } = parse(args)?;
        let state = context.require_state()?;
        let saved = ProviderService::update_pi_usage_script(&state, &id, usage_script)
            .map_err(|error| error.to_string())?;
        serializable(saved)
    })
}

fn get_pi_session_discovery(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(
        async move { serializable(crate::session_manager::providers::pi::session_discovery()) },
    )
}

fn get_usage_first_date(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            app_type: Option<String>,
            provider_name: Option<String>,
            model: Option<String>,
        }
        let Args {
            app_type,
            provider_name,
            model,
        } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            state
                .db
                .get_first_usage_date(
                    app_type.as_deref(),
                    provider_name.as_deref(),
                    model.as_deref(),
                )
                .map_err(|error| error.to_string())
        })
        .await
        .and_then(serializable)
    })
}

// ============================================================================
// Codex 会话压缩 / 过期客户端提示 / 日志文件
// ============================================================================

/// Codex 会话压缩开关（`[features] local_thread_store_compression`）当前是否开启。
fn get_codex_session_compression(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let enabled =
            crate::services::codex_session_compression::is_enabled().map_err(|e| e.to_string())?;
        serializable(enabled)
    })
}

/// 开关 Codex 会话压缩，返回写入后的状态。
fn set_codex_session_compression(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            enabled: bool,
        }
        let Args { enabled } = parse(args)?;
        let state = context.require_state()?;
        // 桌面命令把这一步丢进阻塞线程池（它改 Codex 的配置文件），照此办理
        blocking(move || {
            crate::services::codex_session_compression::set_enabled(&state.db, enabled)
                .map_err(|error| error.to_string())
        })
        .await
        .and_then(serializable)
    })
}

/// Codex 会话目录（sessions + archived_sessions）当前占用的字节数。
fn get_codex_sessions_disk_usage(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    // 目录扫描是同步 IO，丢进阻塞线程池；`deferred` 就是「阻塞 + 序列化」这条
    // 组合，顺带把裸 `u64` 包成 `Ok`
    deferred(|| Ok(crate::services::codex_session_compression::sessions_disk_usage()))
}

/// 用户关掉「Codex 可能还在用旧的」提示：记下当前目录与文件登录，同一份不再提示。
fn acknowledge_codex_stale_clients(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        // `acknowledge` 返回 `()`，别把它绑给变量（clippy 的 `let_unit_value`）；
        // 它要做的事在闭包里：把当前目录与文件登录记进设备存档
        crate::services::provider::codex_direct::off_runtime(|| {
            crate::services::provider::codex_client_catalog::acknowledge(
                &crate::live::engine::DeviceStore::for_device(),
            )
        })
        .await
        .map_err(|error| error.to_string())?;
        Ok(Value::Bool(true))
    })
}

/// 服务端没有桌面会话，替不了**用户**在文件管理器里定位日志文件。
///
/// 与 `open_config_folder` 同一条约定：这类动作在 web 模式下返回描述性错误，
/// 而不是 `E_NOT_IMPLEMENTED` —— 前者能直接告诉用户去哪里找，后者只是「没有
/// 这个命令」。日志就在 `<配置目录>/logs/cc-switch.log`。
fn reveal_app_log_file(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        Err("Web 模式下请在服务器上直接打开 <配置目录>/logs/cc-switch.log".to_string())
    })
}

// ============================================================================
// 备份位置（各处备份的目录与占用）
//
// 与前面几组同理：命令体是「解析 → 调 `services::backup_storage`」，且两次都是
// 同步 IO，桌面版就丢在阻塞线程池里，这里照办。`reveal_backup_location` 走
// `tauri_plugin_opener` 在文件管理器里定位目录，web 模式做不到（服务端没法替
// **用户**弹窗口），与 `reveal_app_log_file` 同一处置。
// ============================================================================

/// 列出本机各类备份的位置与大小。
fn list_backup_locations(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    // 目录扫描是同步 IO；`deferred` 要 `Result`，而 `list_locations` 直接返回
    // Vec，包一层 `Ok` —— 它不失败，只是可能返回空表。
    deferred(|| Ok(crate::services::backup_storage::list_locations()))
}

/// 删除一类备份（按类别 id），返回释放的字节数。
fn delete_backup_location(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            id: String,
        }
        let Args { id } = parse(args)?;
        blocking(move || {
            crate::services::backup_storage::delete_location(&id).map_err(|error| error.to_string())
        })
        .await
        .and_then(serializable)
    })
}

// ============================================================================
// ============================================================================
// Profile（配置集）
//
// 传输形状在 `services::profile_view`，桌面命令与这里共用——它不在 desktop
// 门控里，两边的序列化形状因此不可能漂。
//
// `apply_profile` **必须在阻塞线程池上跑**：桌面命令体的注释写明
// `ProviderService::switch` 内部用 `block_on` 取切换锁，放进 async 命令会在
// 运行时线程上 panic。`blocking()` 正好把它放到阻塞池。
//
// 桌面侧应用完还会重建托盘、并把 Claude Desktop 的映射服务同步过去。前者服务端
// 没有托盘；后者是写外部客户端配置，按决策 #5 一律不接。
// ============================================================================

fn list_profiles(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        blocking(move || {
            let profiles = crate::services::profile::ProfileService::list(&state)
                .map_err(|error| error.to_string())?;
            let current_ids = crate::services::profile_view::CurrentProfileIds {
                claude: state
                    .db
                    .get_current_profile_id(crate::services::profile::ProfileScope::Claude.as_str())
                    .map_err(|error| error.to_string())?,
                claude_desktop: state
                    .db
                    .get_current_profile_id(
                        crate::services::profile::ProfileScope::ClaudeDesktop.as_str(),
                    )
                    .map_err(|error| error.to_string())?,
                codex: state
                    .db
                    .get_current_profile_id(crate::services::profile::ProfileScope::Codex.as_str())
                    .map_err(|error| error.to_string())?,
            };
            Ok(crate::services::profile_view::ProfilesResponse {
                profiles: profiles.into_iter().map(Into::into).collect(),
                current_ids,
            })
        })
        .await
        .and_then(serializable)
    })
}

fn create_profile(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            name: String,
            scope: String,
        }
        let Args { name, scope } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let scope = crate::services::profile::ProfileScope::parse(&scope)
                .map_err(|error| error.to_string())?;
            let profile = crate::services::profile::ProfileService::create(&state, &name, scope)
                .map_err(|error| error.to_string())?;
            serializable(crate::services::profile_view::ProfileDto::from(profile))
        })
        .await
    })
}

fn update_profile(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            id: String,
            name: Option<String>,
            resnapshot: Option<bool>,
            scope: Option<String>,
        }
        let Args {
            id,
            name,
            resnapshot,
            scope,
        } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let scope = scope
                .map(|s| crate::services::profile::ProfileScope::parse(&s))
                .transpose()
                .map_err(|error| error.to_string())?;
            let profile = crate::services::profile::ProfileService::update(
                &state,
                &id,
                name,
                resnapshot.unwrap_or(false),
                scope,
            )
            .map_err(|error| error.to_string())?;
            serializable(crate::services::profile_view::ProfileDto::from(profile))
        })
        .await
    })
}

fn delete_profile(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            id: String,
        }
        let Args { id } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            crate::services::profile::ProfileService::delete(&state, &id)
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(Value::Bool(true))
    })
}

fn clear_current_profile(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            scope: String,
        }
        let Args { scope } = parse(args)?;
        let state = context.require_state()?;
        blocking(move || {
            let scope = crate::services::profile::ProfileScope::parse(&scope)
                .map_err(|error| error.to_string())?;
            state
                .db
                .set_current_profile_id(scope.as_str(), None)
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(Value::Bool(true))
    })
}

/// 应用一份 profile，返回遗留的警告（桌面命令同名同义）。
fn apply_profile(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            id: String,
            scope: String,
        }
        let Args { id, scope } = parse(args)?;
        let state = context.require_state()?;

        // `ProviderService::switch` 内部 `block_on` 取切换锁，在 async 任务里跑
        // 会在运行时线程上 panic —— 整段丢进阻塞池。
        let outcome = blocking(move || {
            let scope = crate::services::profile::ProfileScope::parse(&scope)
                .map_err(|error| error.to_string())?;
            let warnings = crate::services::profile::ProfileService::apply(&state, &id, scope)
                .map_err(|error| error.to_string())?;
            // 事件形状与桌面一致，只是换条通道发：服务端没有 AppHandle，走
            // event_sink，前端拿到的 payload 一模一样。
            for (name, payload) in crate::services::profile_view::apply_events(&state, &id, scope) {
                crate::event_sink::emit(name, payload);
            }
            // 桌面侧还会重建托盘、把 Claude Desktop 的映射服务同步过去：前者
            // 服务端没有托盘，后者是写外部客户端配置（决策 #5 不接）。
            Ok(warnings)
        })
        .await?;
        serializable(outcome)
    })
}

// ============================================================================
// WebDAV 同步
//
// 流程胶水在 `services::webdav_flow`（原先住在 desktop 门控的命令层里，两边
// 现在共用同一份）；真正做网络 IO 的部分本来就在 `services::webdav_sync`。
// 全局同步互斥锁也在那边——两个恢复同时跑会把文件写成半新半旧，所以它只该
// 有一处定义，这里只是去取。
// ============================================================================

fn webdav_test_connection(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            settings: crate::settings::WebDavSyncSettings,
            preserve_empty_password: Option<bool>,
        }
        let Args {
            settings,
            preserve_empty_password,
        } = parse(args)?;

        // 空密码有两种意思：没改过 / 清空。这里按桌面命令的默认走「没改过」。
        let preserve_empty = preserve_empty_password.unwrap_or(true);
        let resolved = crate::services::webdav_flow::resolve_password_for_request(
            settings,
            crate::settings::get_webdav_sync_settings(),
            preserve_empty,
        );
        crate::services::webdav_sync::check_connection(&resolved)
            .await
            .map_err(|error| error.to_string())?;
        Ok(serde_json::json!({ "success": true, "message": "WebDAV connection ok" }))
    })
}

fn webdav_sync_upload(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let db = state.db.clone();
        let mut settings = crate::services::webdav_flow::require_enabled_settings()?;

        let result = crate::services::webdav_flow::run_with_lock(
            crate::services::webdav_sync::upload(&db, &mut settings),
        )
        .await;
        let value = crate::services::webdav_flow::map_sync_result(result, |error| {
            crate::services::webdav_flow::persist_sync_error(&mut settings, error, "manual")
        })?;
        serializable(value)
    })
}

fn webdav_sync_download(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let db = state.db.clone();
        let app_state_for_sync = state.clone();
        let mut settings = crate::services::webdav_flow::require_enabled_settings()?;

        // 下载与「把快照投影成各处 live 文件」必须同持一把锁：否则另一轮恢复
        // 可能插进「库已应用、投影没跑完」的窗口。桌面命令就是这么做的。
        let sync_result = crate::services::webdav_flow::run_download_with_lock(
            crate::services::webdav_sync::download(&db, &mut settings),
            |result| async move {
                let post_sync_result = crate::host::spawn_blocking(move || {
                    crate::services::sync_support::run_post_import_sync(&app_state_for_sync)
                })
                .await
                .map_err(|e| e.to_string());
                Ok((result, post_sync_result))
            },
        )
        .await;
        let (mut result, post_sync_result) =
            crate::services::webdav_flow::map_sync_result(sync_result, |error| {
                crate::services::webdav_flow::persist_sync_error(&mut settings, error, "manual")
            })?;

        // 投影是尽力而为：快照恢复已经成功了，失败只作为警告附在结果上。
        let warning =
            crate::services::sync_support::post_sync_warning_from_result(post_sync_result);
        if let Some(message) = warning.as_ref() {
            log::warn!("[WebDAV] post-download sync warning: {message}");
        }
        result = crate::services::sync_support::attach_warning(result, warning);
        serializable(result)
    })
}

fn webdav_sync_save_settings(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            settings: crate::settings::WebDavSyncSettings,
            password_touched: Option<bool>,
        }
        let Args {
            settings,
            password_touched,
        } = parse(args)?;

        let password_touched = password_touched.unwrap_or(false);
        let existing = crate::settings::get_webdav_sync_settings();
        let mut sync_settings = crate::services::webdav_flow::resolve_password_for_request(
            settings,
            existing.clone(),
            !password_touched,
        );

        // 保留前端不管理的字段（同步状态由服务端写）
        if let Some(existing_settings) = existing {
            sync_settings.status = existing_settings.status;
        }

        sync_settings.normalize();
        sync_settings
            .validate()
            .map_err(|error| error.to_string())?;
        crate::settings::set_webdav_sync_settings(Some(sync_settings))
            .map_err(|error| error.to_string())?;
        Ok(serde_json::json!({ "success": true }))
    })
}

fn webdav_sync_fetch_remote_info(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let settings = crate::services::webdav_flow::require_enabled_settings()?;
        let info = crate::services::webdav_sync::fetch_remote_info(&settings)
            .await
            .map_err(|error| error.to_string())?;
        Ok(info.unwrap_or_else(|| serde_json::json!({ "empty": true })))
    })
}

// ============================================================================
// S3 同步
//
// 与上面的 WebDAV 一组同构：流程胶水在 `services::s3_flow`，真正做网络 IO 的
// 部分在 `services::s3_sync`，全局同步互斥锁也在后者——两个恢复同时跑会把文件
// 写成半新半旧，所以它只该有一处定义，这里只是去取。
// ============================================================================

fn s3_test_connection(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            settings: crate::settings::S3SyncSettings,
            preserve_empty_secret: Option<bool>,
        }
        let Args {
            settings,
            preserve_empty_secret,
        } = parse(args)?;

        // 留空的 secret 有两种意思：没改过 / 清空。按桌面命令的默认走「没改过」。
        let preserve_empty = preserve_empty_secret.unwrap_or(true);
        let resolved = crate::services::s3_flow::resolve_secret_for_request(
            settings,
            crate::settings::get_s3_sync_settings(),
            preserve_empty,
        );
        crate::services::s3_sync::check_connection(&resolved)
            .await
            .map_err(|error| error.to_string())?;
        Ok(serde_json::json!({ "success": true, "message": "S3 connection ok" }))
    })
}

fn s3_sync_upload(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let db = state.db.clone();
        let mut settings = crate::services::s3_flow::require_enabled_settings()?;

        let result = crate::services::s3_flow::run_with_lock(crate::services::s3_sync::upload(
            &db,
            &mut settings,
        ))
        .await;
        let value = crate::services::s3_flow::map_sync_result(result, |error| {
            crate::services::s3_flow::persist_sync_error(&mut settings, error, "manual")
        })?;
        serializable(value)
    })
}

fn s3_sync_download(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let db = state.db.clone();
        let app_state_for_sync = state.clone();
        let mut settings = crate::services::s3_flow::require_enabled_settings()?;

        // 下载与投影同持一把锁，理由与 WebDAV 那组相同。
        let sync_result = crate::services::s3_flow::run_download_with_lock(
            crate::services::s3_sync::download(&db, &mut settings),
            |result| async move {
                let post_sync_result = crate::host::spawn_blocking(move || {
                    crate::services::sync_support::run_post_import_sync(&app_state_for_sync)
                })
                .await
                .map_err(|e| e.to_string());
                Ok((result, post_sync_result))
            },
        )
        .await;
        let (mut result, post_sync_result) =
            crate::services::s3_flow::map_sync_result(sync_result, |error| {
                crate::services::s3_flow::persist_sync_error(&mut settings, error, "manual")
            })?;

        // 投影是尽力而为：快照恢复已经成功了，失败只作为警告附在结果上。
        let warning =
            crate::services::sync_support::post_sync_warning_from_result(post_sync_result);
        if let Some(message) = warning.as_ref() {
            log::warn!("[S3] post-download sync warning: {message}");
        }
        result = crate::services::sync_support::attach_warning(result, warning);
        serializable(result)
    })
}

fn s3_sync_save_settings(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            settings: crate::settings::S3SyncSettings,
            secret_touched: Option<bool>,
        }
        let Args {
            settings,
            secret_touched,
        } = parse(args)?;

        let secret_touched = secret_touched.unwrap_or(false);
        let existing = crate::settings::get_s3_sync_settings();
        let mut sync_settings = crate::services::s3_flow::resolve_secret_for_request(
            settings,
            existing.clone(),
            !secret_touched,
        );

        // 保留前端不管理的字段（同步状态由服务端写）
        if let Some(existing_settings) = existing {
            sync_settings.status = existing_settings.status;
        }

        sync_settings.normalize();
        sync_settings
            .validate()
            .map_err(|error| error.to_string())?;
        crate::settings::set_s3_sync_settings(Some(sync_settings))
            .map_err(|error| error.to_string())?;
        Ok(serde_json::json!({ "success": true }))
    })
}

fn s3_sync_fetch_remote_info(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let settings = crate::services::s3_flow::require_enabled_settings()?;
        let info = crate::services::s3_sync::fetch_remote_info(&settings)
            .await
            .map_err(|error| error.to_string())?;
        Ok(info.unwrap_or_else(|| serde_json::json!({ "empty": true })))
    })
}

// ============================================================================
// 用量额度：余额 / 编码套餐 / 订阅
//
// 这三条都是「解析参数 → 调 service」，service 全在非门控模块里，原样搬。
//
// `get_subscription_quota` 桌面版还做两件收尾：把快照写进 usage_cache、并经
// AppHandle 发 `usage-cache-updated`。缓存在 `AppState` 里（两边同源），事件
// 换 `event_sink` 发——payload 一字不改，前端 `useUsageCacheBridge` 依赖它。
//
// 注释里那条语义值得留住：**瞬时传输失败不写缓存、不发事件**，保留上一份托盘
// 快照，与前端 react-query「reject 保留上次 data」一致；认证/HTTP 失败才替换
// 快照，好让界面把失效的额度收起来。
// ============================================================================

fn get_subscription_quota(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            tool: String,
        }
        let Args { tool } = parse(args)?;
        let state = context.require_state()?;

        let inner = crate::services::subscription::get_subscription_quota(&tool).await;
        if let Ok(snapshot) = &inner {
            if let Ok(app_type) = crate::app_config::AppType::from_str(&tool) {
                // `AppType` 不是 `Copy`：先把 `as_str` 取下来，再把值移进缓存
                let app_str = app_type.as_str().to_string();
                state
                    .usage_cache
                    .put_subscription(app_type, snapshot.clone());
                crate::event_sink::emit(
                    "usage-cache-updated",
                    serde_json::json!({
                        "kind": "subscription",
                        "appType": app_str,
                        "data": snapshot,
                    }),
                );
            }
        }
        inner.and_then(serializable)
    })
}

/// 供应商余额查询（按 base_url + api_key，无 state）。
fn get_balance(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            base_url: String,
            api_key: String,
        }
        let Args { base_url, api_key } = parse(args)?;
        let result = crate::services::balance::get_balance(&base_url, &api_key).await?;
        serializable(result)
    })
}

/// 编码套餐额度查询。火山方舟用 AK/SK 签名查询，其他供应商走 api_key。
fn get_coding_plan_quota(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            base_url: String,
            api_key: String,
            access_key_id: Option<String>,
            secret_access_key: Option<String>,
            coding_plan_provider: Option<String>,
            team_organization_id: Option<String>,
            team_project_id: Option<String>,
        }
        let Args {
            base_url,
            api_key,
            access_key_id,
            secret_access_key,
            coding_plan_provider,
            team_organization_id,
            team_project_id,
        } = parse(args)?;
        let quota = crate::services::coding_plan::get_coding_plan_quota(
            &base_url,
            &api_key,
            access_key_id.as_deref(),
            secret_access_key.as_deref(),
            coding_plan_provider.as_deref(),
            team_organization_id.as_deref(),
            team_project_id.as_deref(),
        )
        .await?;
        serializable(quota)
    })
}

// 导入导出与数据库备份
//
// 桌面的文件对话框在浏览器里没有对应物：shim 用 `open_file_dialog` 选择文件后
// POST 到 `/api/upload`，拿到**服务端路径**再交给下面这些命令；导出方向则由
// `web_allocate_export_path` 分配一个服务端路径，导出成功后 shim 触发
// `/api/download`。
// ============================================================================

/// 为一次导出分配服务端路径（仅前端 shim 使用）。
fn web_allocate_export_path(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            default_name: String,
        }
        let Args { default_name } = parse(args)?;

        // 只取文件名部分，杜绝 `../` 穿越
        let file_name = std::path::Path::new(&default_name)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "cc-switch-export.sql".to_string());

        let dir = crate::config::get_app_config_dir().join("exports");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
        let path = dir.join(file_name);
        Ok(Value::String(path.to_string_lossy().into_owned()))
    })
}

fn export_config_to_file(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            file_path: String,
        }
        let Args { file_path } = parse(args)?;
        let state = context.require_state()?;
        crate::services::import_export::export_to_file(&state, &file_path).await
    })
}

fn import_config_from_file(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            file_path: String,
        }
        let Args { file_path } = parse(args)?;
        let state = context.require_state()?;
        crate::services::import_export::import_from_file(&state, &file_path).await
    })
}

fn create_db_backup(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let state = context.require_state()?;
        let file_name = crate::services::import_export::create_backup(&state).await?;
        Ok(Value::String(file_name))
    })
}

fn list_db_backups(context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        context.require_state()?;
        let backups = crate::services::import_export::list_backups()?;
        serializable(backups)
    })
}

fn restore_db_backup(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            filename: String,
        }
        let Args { filename } = parse(args)?;
        let state = context.require_state()?;
        let restored = crate::services::import_export::restore_backup(&state, &filename).await?;
        Ok(Value::String(restored))
    })
}

fn rename_db_backup(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            old_filename: String,
            new_name: String,
        }
        let Args {
            old_filename,
            new_name,
        } = parse(args)?;
        context.require_state()?;
        let renamed = crate::services::import_export::rename_backup(&old_filename, &new_name)?;
        Ok(Value::String(renamed))
    })
}

fn delete_db_backup(context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            filename: String,
        }
        let Args { filename } = parse(args)?;
        context.require_state()?;
        crate::services::import_export::delete_backup(&filename)?;
        Ok(Value::Bool(true))
    })
}

// ============================================================================
// 会话浏览
//
// 读的是**服务端这台机器**上各 CLI 工具留下的会话文件——在服务器上跑 CLI
// 的场景里这正是要看的东西。桌面的终端/文件管理器相关命令在服务端没有对应物。
// ============================================================================

fn list_sessions(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        let sessions = blocking(|| Ok(crate::session_manager::scan_sessions())).await?;
        serializable(sessions)
    })
}

fn get_session_messages(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            provider_id: String,
            source_path: String,
        }
        let Args {
            provider_id,
            source_path,
        } = parse(args)?;

        blocking(move || {
            crate::session_manager::load_transcript(&provider_id, &source_path)
                .map(|loaded| loaded.transcript.messages.clone())
        })
        .await
        .and_then(serializable)
    })
}

/// 一次性返回整段会话（含轮次索引）。
///
/// 浏览器端不支持 Tauri 的 `Channel`，`stream_session_messages` 由前端 shim 用
/// 这个命令的数据在浏览器里分块喂给回调——分块逻辑在浏览器侧做，服务端只读一次。
fn web_session_transcript(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            provider_id: String,
            source_path: String,
        }
        let Args {
            provider_id,
            source_path,
        } = parse(args)?;

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Payload {
            messages: Vec<crate::session_manager::SessionMessage>,
            turns: Vec<crate::session_manager::model::TurnIndex>,
            approx_bytes: u64,
            cached: bool,
            parse_ms: u64,
        }

        blocking(move || {
            let loaded = crate::session_manager::load_transcript(&provider_id, &source_path)?;
            Ok(Payload {
                messages: loaded.transcript.messages.clone(),
                turns: loaded.transcript.turns.clone(),
                approx_bytes: loaded.transcript.approx_bytes as u64,
                cached: loaded.cached,
                parse_ms: loaded.parse_ms,
            })
        })
        .await
        .and_then(serializable)
    })
}

fn get_session_block_content(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            provider_id: String,
            source_path: String,
            content_ref: crate::session_manager::model::ContentRef,
            offset: Option<u32>,
            limit: Option<u32>,
        }
        let Args {
            provider_id,
            source_path,
            content_ref,
            offset,
            limit,
        } = parse(args)?;

        blocking(move || {
            let source =
                crate::session_manager::content::validate_source(&provider_id, &source_path)?;
            let full = crate::session_manager::content::resolve_content_ref(&source, &content_ref)?;
            Ok(crate::session_manager::content::paginate(
                &full, offset, limit,
            ))
        })
        .await
        .and_then(serializable)
    })
}

fn delete_session(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            provider_id: String,
            session_id: String,
            source_path: String,
        }
        let Args {
            provider_id,
            session_id,
            source_path,
        } = parse(args)?;

        blocking(move || {
            crate::session_manager::delete_session(&provider_id, &session_id, &source_path)
        })
        .await
        .and_then(serializable)
    })
}

fn delete_sessions(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        struct Args {
            items: Vec<crate::session_manager::DeleteSessionRequest>,
        }
        let Args { items } = parse(args)?;
        blocking(move || Ok(crate::session_manager::delete_sessions(&items)))
            .await
            .and_then(serializable)
    })
}

/// 导出会话为 Markdown：服务端写进 `<配置目录>/exports/`，前端随后下载
/// （与配置导出一致，见 `web_allocate_export_path`）。
fn export_session_markdown(_context: Arc<Context>, args: Value) -> HandlerFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            default_name: String,
            content: String,
        }
        let Args {
            default_name,
            content,
        } = parse(args)?;

        let file_name = std::path::Path::new(&default_name)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "session.md".to_string());

        let dir = crate::config::get_app_config_dir().join("exports");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
        let path = dir.join(file_name);
        crate::config::write_text_file(&path, &content).map_err(|e| e.to_string())?;
        Ok(Value::String(path.to_string_lossy().into_owned()))
    })
}

/// 在文件管理器中显示路径：服务端没有桌面会话。
fn reveal_session_path(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        Err("Web 模式下没有文件管理器，请在服务器上直接查看该路径".to_string())
    })
}

/// 在终端里恢复会话：服务端没有桌面终端。
fn launch_session_terminal(_context: Arc<Context>, _args: Value) -> HandlerFuture {
    Box::pin(async move {
        Err("Web 模式下不能打开终端；请在服务器上用 CLI 自行恢复会话".to_string())
    })
}
