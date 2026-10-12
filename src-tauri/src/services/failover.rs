//! 故障转移队列的业务逻辑。
//!
//! 桌面命令层（`commands/failover.rs`）与服务端（`web/routes.rs`）共用这里的实现：
//! 命令层只多做一件事——刷新托盘菜单；其余（校验、入队/出队、开启时把 P1 切上去、
//! 失败回滚）都在这里，避免两份实现漂移。

use std::str::FromStr;

use crate::app_config::AppType;
use crate::database::FailoverQueueItem;
use crate::provider::Provider;
use crate::store::AppState;

/// 只有支持本地路由的应用才有故障转移。
pub fn require_failover_app(app_type: &str) -> Result<(), String> {
    let app = AppType::from_str(app_type).map_err(|error| format!("无效的应用类型: {error}"))?;
    if !app.supports_local_proxy() {
        return Err(format!("{} 不支持故障转移", app.as_str()));
    }
    Ok(())
}

/// 校验供应商存在且支持故障转移（Codex 官方账号卡不支持）。
pub fn require_failover_provider(
    db: &crate::database::Database,
    app_type: &str,
    provider_id: &str,
) -> Result<Provider, String> {
    let provider = db
        .get_provider_by_id(provider_id, app_type)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("供应商不存在: {provider_id}"))?;
    if !crate::proxy::provider_router::provider_supports_failover(app_type, &provider) {
        return Err("Codex Official 账号卡不支持自动故障转移".to_string());
    }
    Ok(provider)
}

// ============================================================================
// 队列
// ============================================================================

pub async fn get_queue(state: &AppState, app_type: &str) -> Result<Vec<FailoverQueueItem>, String> {
    require_failover_app(app_type)?;
    let queue = state
        .db
        .get_failover_queue(app_type)
        .map_err(|e| e.to_string())?;
    if app_type != "codex" {
        return Ok(queue);
    }

    // Codex 官方账号卡留在队列里没有意义，读取时过滤掉
    let providers = state
        .db
        .get_all_providers(app_type)
        .map_err(|e| e.to_string())?;
    Ok(filter_supported(app_type, queue, &providers))
}

pub async fn get_available_providers(
    state: &AppState,
    app_type: &str,
) -> Result<Vec<Provider>, String> {
    require_failover_app(app_type)?;
    let providers = state
        .db
        .get_available_providers_for_failover(app_type)
        .map_err(|e| e.to_string())?;
    Ok(providers
        .into_iter()
        .filter(|provider| {
            crate::proxy::provider_router::provider_supports_failover(app_type, provider)
        })
        .collect())
}

pub async fn add(state: &AppState, app_type: &str, provider_id: &str) -> Result<(), String> {
    require_failover_app(app_type)?;
    require_failover_provider(&state.db, app_type, provider_id)?;
    state
        .db
        .add_to_failover_queue(app_type, provider_id)
        .map_err(|e| e.to_string())
}

pub async fn remove(state: &AppState, app_type: &str, provider_id: &str) -> Result<(), String> {
    require_failover_app(app_type)?;
    state
        .db
        .remove_from_failover_queue(app_type, provider_id)
        .map_err(|e| e.to_string())
}

pub async fn auto_failover_enabled(state: &AppState, app_type: &str) -> Result<bool, String> {
    require_failover_app(app_type)?;
    state
        .db
        .get_proxy_config_for_app(app_type)
        .await
        .map(|config| config.auto_failover_enabled)
        .map_err(|e| e.to_string())
}

/// 设置自动故障转移开关。
///
/// 开启时：队列为空会把当前供应商自动加为 P1（避免"必须先加队列才能开启"的死锁），
/// 然后**先切到 P1 再写开关**——P1 切不过去（例如官方卡）时不留"开关已开但没切"的脏状态，
/// 自动加入的 P1 也会回滚。
///
/// 返回开启时的 P1 供应商 ID（调用方据此发 `provider-switched` 事件）；关闭时返回 `None`。
pub async fn set_auto_failover_enabled(
    state: &AppState,
    app_type: &str,
    enabled: bool,
) -> Result<Option<String>, String> {
    require_failover_app(app_type)?;
    let app_enum =
        AppType::from_str(app_type).map_err(|_| format!("无效的应用类型: {app_type}"))?;
    log::info!(
        "[Failover] Setting auto_failover_enabled: app_type='{app_type}', enabled={enabled}"
    );

    let mut config = state
        .db
        .get_proxy_config_for_app(app_type)
        .await
        .map_err(|e| e.to_string())?;

    if enabled && !crate::mode::current::is_proxy(&app_enum) {
        return Err("需要先让该应用进入路由模式，再开启故障转移".to_string());
    }

    let mut auto_added_provider_id: Option<String> = None;
    let p1_provider_id = if enabled {
        let all_providers = state
            .db
            .get_all_providers(app_type)
            .map_err(|e| e.to_string())?;
        let mut queue = filter_supported(
            app_type,
            state
                .db
                .get_failover_queue(app_type)
                .map_err(|e| e.to_string())?,
            &all_providers,
        );

        if queue.is_empty() {
            let current_id = crate::mode::current::provider_for(
                &state.db,
                &app_enum,
                crate::mode::current::Purpose::InUse,
            )
            .map_err(|e| e.to_string())?;

            let Some(current_id) = current_id else {
                return Err("故障转移队列为空，且未设置当前供应商，无法开启故障转移".to_string());
            };

            require_failover_provider(&state.db, app_type, &current_id)?;

            state
                .db
                .add_to_failover_queue(app_type, &current_id)
                .map_err(|e| e.to_string())?;
            auto_added_provider_id = Some(current_id);

            queue = filter_supported(
                app_type,
                state
                    .db
                    .get_failover_queue(app_type)
                    .map_err(|e| e.to_string())?,
                &all_providers,
            );
        }

        queue
            .first()
            .map(|item| item.provider_id.clone())
            .ok_or_else(|| "故障转移队列为空，无法开启故障转移".to_string())?
    } else {
        String::new()
    };

    if enabled {
        // Stack 模式不做故障转移，切换会在锁内被拒绝（见 `switch_route_for_failover`）。
        if let Err(error) =
            crate::mode::controller::switch_route_for_failover(state, &app_enum, &p1_provider_id)
                .await
        {
            if let Some(provider_id) = auto_added_provider_id {
                let _ = state.db.remove_from_failover_queue(app_type, &provider_id);
            }
            return Err(error);
        }
    }

    // `enabled` 是模式的镜像，保持与模式一致
    config.auto_failover_enabled = enabled;
    config.enabled = crate::mode::current::is_proxy(&app_enum);

    state
        .db
        .update_proxy_config_for_app(config)
        .await
        .map_err(|e| e.to_string())?;

    Ok(enabled.then_some(p1_provider_id))
}

fn filter_supported(
    app_type: &str,
    queue: Vec<FailoverQueueItem>,
    providers: &indexmap::IndexMap<String, Provider>,
) -> Vec<FailoverQueueItem> {
    queue
        .into_iter()
        .filter(|item| {
            providers.get(&item.provider_id).is_some_and(|provider| {
                crate::proxy::provider_router::provider_supports_failover(app_type, provider)
            })
        })
        .collect()
}

/// 重置某个供应商的熔断器；若它恢复后在队列里优先级更高，把路由切回它。
///
/// 桌面命令层（`commands/proxy.rs`）与服务端（`web/routes.rs`）共用这一份实现。
/// 命令体里不是纯粹的"改一行状态"：它带一个真实分支——恢复的供应商要不要
/// 回切——要对照 `sort_index`、自动故障转移开关与代理是否在跑。这种逻辑
/// 拷两份一定会漂移，所以按本文件的约定搬到这里。
///
/// 顺序与上游一致：先写数据库健康状态 → 再清代理内存里的熔断器 → 最后才
/// 可能回切。前三步即使不做回切也必须发生。
pub async fn reset_circuit_breaker(
    state: &AppState,
    provider_id: &str,
    app_type: &str,
) -> Result<(), String> {
    // 入参就地升成 String，好让下面每一处 `&x` 与上游命令体里的类型一模一样，
    // 内层 API 要 `&String` 还是 `&str` 都不必再逐个推断。
    let provider_id = provider_id.to_string();
    let app_type = app_type.to_string();

    require_failover_app(&app_type)?;
    let app = AppType::from_str(&app_type).map_err(|error| error.to_string())?;
    let db = &state.db;

    // 1. 重置数据库健康状态
    db.update_provider_health(&provider_id, &app_type, true, None)
        .await
        .map_err(|error| error.to_string())?;

    // 2. 代理正在跑的话，同时清掉它内存里的熔断器状态
    state
        .proxy_service
        .reset_provider_circuit_breaker(&provider_id, &app_type)
        .await?;

    // 3. 只有「处于代理模式 + 开了自动故障转移 + 代理确实在跑」三条同时成立
    //    才谈得上回切；任一不成立时，前两步的重置依然有效，直接结束。
    let app_in_proxy = crate::mode::current::is_proxy(&app);
    let auto_failover_enabled = match db.get_proxy_config_for_app(&app_type).await {
        Ok(config) => config.auto_failover_enabled,
        Err(error) => {
            log::error!(
                "[{app_type}] Failed to read proxy_config: {error}, defaulting to disabled"
            );
            false
        }
    };

    if !app_in_proxy || !auto_failover_enabled || !state.proxy_service.is_running().await {
        return Ok(());
    }

    let Some(current_id) =
        crate::mode::current::provider_for(db, &app, crate::mode::current::Purpose::InUse)
            .map_err(|error| error.to_string())?
    else {
        return Ok(());
    };

    let queue = db
        .get_failover_queue(&app_type)
        .map_err(|error| error.to_string())?;
    // 两边都用 sort_index 定位：它是队列序，也是优先级（P1 比 P2 先试）
    let order_of = |id: &str| {
        queue
            .iter()
            .find(|item| item.provider_id == id)
            .and_then(|item| item.sort_index)
    };
    let (Some(restored), Some(current)) = (order_of(&provider_id), order_of(&current_id)) else {
        return Ok(());
    };
    if restored >= current {
        return Ok(());
    }

    let provider_name = db
        .get_all_providers(&app_type)
        .ok()
        .and_then(|providers| providers.get(&provider_id).map(|p| p.name.clone()))
        .unwrap_or_else(|| provider_id.clone());

    log::info!(
        "[Recovery] 供应商 {provider_id} 已恢复且优先级更高 (P{restored} vs P{current})，自动切换"
    );
    let switch_manager = crate::proxy::failover_switch::FailoverSwitchManager::new();
    if let Err(error) = switch_manager
        .try_switch(&app_type, &provider_id, &provider_name)
        .await
    {
        log::error!("[Recovery] 自动切换失败: {error}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{require_failover_app, require_failover_provider};
    use crate::database::Database;
    use crate::provider::{AuthBinding, AuthBindingSource, Provider, ProviderMeta};
    use serde_json::json;

    #[test]
    fn failover_rejects_apps_without_a_proxy_data_plane() {
        assert!(require_failover_app("claude").is_ok());
        assert!(require_failover_app("pi").is_err());
    }

    #[test]
    fn failover_rejects_codex_official_account_cards() {
        let db = Database::memory().expect("memory db");
        let mut official = Provider::with_id(
            "official-a".to_string(),
            "OpenAI Official".to_string(),
            json!({ "auth": {}, "config": "" }),
            None,
        );
        official.category = Some("official".to_string());
        official.meta = Some(ProviderMeta {
            auth_binding: Some(AuthBinding {
                source: AuthBindingSource::ManagedAccount,
                auth_provider: Some("codex_oauth".to_string()),
                account_id: Some("account-a".to_string()),
            }),
            ..Default::default()
        });
        db.save_provider("codex", &official).expect("save official");

        assert!(require_failover_provider(&db, "codex", &official.id).is_err());
    }
}
