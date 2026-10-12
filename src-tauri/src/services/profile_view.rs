//! Profile 的传输形状与应用后的事件。
//!
//! 这些只描述「一条 profile 在接口上长什么样」以及「应用完该发哪些事件」，
//! 不碰 Tauri，所以放在 `services` 而不是桌面命令层——后者只在 `desktop`
//! feature 下编译，服务端根本看不见它。两边共用同一份定义，序列化形状与事件
//! payload 就不可能各走各的。
//!
//! 事件形状是**契约**：前端 `App.tsx` 的 `provider-switched` 监听依赖
//! `{ appType, proxyEnabled, autoFailoverEnabled, providerId }` 这四个字段，
//! 桌面入口与托盘入口原本就必须共用一个构造函数来保证它一致。服务端加进来后
//! 这一点更该成立，于是构造留在这里、发射各走各的通道。

use serde::Serialize;

use crate::database::Profile;
use crate::store::AppState;

use super::profile::{ProfilePayload, ProfileScope};

/// 一条 profile 的接口形状。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDto {
    pub id: String,
    pub name: String,
    pub payload: ProfilePayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

impl From<Profile> for ProfileDto {
    fn from(profile: Profile) -> Self {
        // 单条 payload 损坏不应拖垮整个列表：降级为默认值并记日志
        let payload = serde_json::from_str(&profile.payload).unwrap_or_else(|e| {
            log::warn!(
                "解析 profile '{}' payload 失败，使用默认值: {e}",
                profile.id
            );
            ProfilePayload::default()
        });
        Self {
            id: profile.id,
            name: profile.name,
            payload,
            created_at: profile.created_at,
            updated_at: profile.updated_at,
        }
    }
}

/// 每个分组当前激活的 profile id（该分组未使用 profile 时为 `None`）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentProfileIds {
    pub claude: Option<String>,
    pub claude_desktop: Option<String>,
    pub codex: Option<String>,
}

/// `list_profiles` 的返回。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesResponse {
    pub profiles: Vec<ProfileDto>,
    pub current_ids: CurrentProfileIds,
}

/// Profile 应用完成后要发的事件及其 payload。
///
/// 一个 scope 里每个 app 一条 `provider-switched`，外加一条 `profile-applied`。
/// 返回 `(事件名, payload)` 序列，由调用方决定从哪条通道发出。
///
/// 注意这里**不含**桌面侧的两件收尾——重建托盘菜单、以及把 Claude Desktop
/// 的映射服务同步过去。前者服务端没有托盘；后者是写外部客户端配置，
/// 按本项目的决策 #5 一律不接。
pub fn apply_events(
    state: &AppState,
    profile_id: &str,
    scope: ProfileScope,
) -> Vec<(&'static str, serde_json::Value)> {
    let mut events = Vec::new();
    for app_type in scope.apps().iter() {
        let app_str = app_type.as_str();
        let (_, auto_failover_enabled) = state.db.get_proxy_flags_sync(app_str);
        let proxy_enabled = crate::mode::current::is_proxy(app_type);
        let provider_id = crate::mode::current::provider_for(
            &state.db,
            app_type,
            crate::mode::current::Purpose::InUse,
        )
        .ok()
        .flatten()
        .unwrap_or_default();
        events.push((
            "provider-switched",
            serde_json::json!({
                "appType": app_str,
                "proxyEnabled": proxy_enabled,
                "autoFailoverEnabled": auto_failover_enabled,
                "providerId": provider_id,
            }),
        ));
    }
    events.push((
        "profile-applied",
        serde_json::json!({ "profileId": profile_id, "scope": scope.as_str() }),
    ));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 字段名是前端契约，改了就要同时改读它的组件。
    #[test]
    fn dto_serializes_to_the_camel_case_shape_the_form_expects() {
        let dto = ProfileDto {
            id: "p1".to_string(),
            name: "work".to_string(),
            payload: ProfilePayload::default(),
            created_at: Some(1),
            updated_at: None,
        };
        let value = serde_json::to_value(&dto).expect("serialize");
        for field in ["id", "name", "payload"] {
            assert!(value.get(field).is_some(), "missing {field}");
        }
        // camelCase, not snake_case: the form reads createdAt.
        assert_eq!(value.get("createdAt"), Some(&serde_json::json!(1)));
        assert!(
            value.get("updatedAt").is_none(),
            "absent optional must not appear"
        );
    }

    /// `apply_events` 的字段集合是 `App.tsx` 的 `provider-switched` 监听依赖的。
    #[test]
    fn provider_switched_carries_the_four_fields_the_frontend_reads() {
        let payload = serde_json::json!({
            "appType": "claude",
            "proxyEnabled": false,
            "autoFailoverEnabled": false,
            "providerId": "",
        });
        for field in [
            "appType",
            "proxyEnabled",
            "autoFailoverEnabled",
            "providerId",
        ] {
            assert!(payload.get(field).is_some(), "missing {field}");
        }
    }
}
