//! 全局出站代理的业务逻辑。
//!
//! 桌面命令层（`commands/global_proxy.rs`）与服务端（`web/routes.rs`）共用这里的
//! 实现。原先它整段住在命令层里，而那个模块只在 `desktop` feature 下编译——服务端
//! 因此看不见这些类型，也就没法把它们原样送回浏览器。按本层的约定搬到这里：
//! 命令层只多做一件事（刷托盘），其余都在 `services/`，避免两份实现漂移。
//!
//! 其中 `test_proxy_url` 与 `scan_local_proxies` 带真实逻辑（一次外网探测、
//! 一轮本地端口扫描），这类逻辑尤其不该拷两份。

use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::proxy::http_client;
use crate::store::AppState;

/// 代理测试结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResult {
    /// 是否连接成功
    pub success: bool,
    /// 延迟（毫秒）
    pub latency_ms: u64,
    /// 错误信息
    pub error: Option<String>,
}

/// 出站代理状态信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamProxyStatus {
    /// 是否启用代理
    pub enabled: bool,
    /// 代理 URL
    pub proxy_url: Option<String>,
}

/// 检测到的代理信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedProxy {
    /// 代理 URL
    pub url: String,
    /// 代理类型 (http/socks5)
    pub proxy_type: String,
    /// 端口
    pub port: u16,
}

/// 常见代理端口配置
/// 格式：(端口, 主要类型, 是否同时支持 http 和 socks5)
/// 对于 mixed 端口，会同时返回两种协议供用户选择
const PROXY_PORTS: &[(u16, &str, bool)] = &[
    (7890, "http", true),     // Clash (mixed mode)
    (7891, "socks5", false),  // Clash SOCKS only
    (1080, "socks5", false),  // 通用 SOCKS5
    (8080, "http", false),    // 通用 HTTP
    (8888, "http", false),    // Charles/Fiddler
    (3128, "http", false),    // Squid
    (10808, "socks5", false), // V2Ray SOCKS
    (10809, "http", false),   // V2Ray HTTP
];

/// 读全局代理 URL，`None` 表示直连。
pub fn get_global_proxy_url(state: &AppState) -> Result<Option<String>, String> {
    let result = state.db.get_global_proxy_url().map_err(|e| e.to_string())?;
    log::debug!(
        "[GlobalProxy] [GP-010] Read from database: {}",
        result
            .as_ref()
            .map(|u| http_client::mask_url(u))
            .unwrap_or_else(|| "None".to_string())
    );
    Ok(result)
}

/// 写全局代理 URL：非空启用代理，空串清除回直连。
///
/// 顺序是先验证 → 再落库 → 最后应用，保证库写失败时不会出现
/// 「运行态已变、持久化没跟上」的不一致。
pub fn set_global_proxy_url(state: &AppState, url: String) -> Result<(), String> {
    // 只记长度与是否带凭据，绝不把 URL 本体写进日志
    let has_auth = url.contains('@') && (url.starts_with("http://") || url.starts_with("socks"));
    log::debug!(
        "[GlobalProxy] [GP-011] Received URL: length={}, has_auth={}",
        url.len(),
        has_auth
    );

    let url_opt = if url.trim().is_empty() {
        None
    } else {
        Some(url.as_str())
    };

    // 1. 先验证代理配置是否有效（不应用）
    http_client::validate_proxy(url_opt)?;

    // 2. 验证成功后保存到数据库
    state
        .db
        .set_global_proxy_url(url_opt)
        .map_err(|e| e.to_string())?;

    // 3. DB 写入成功后再应用到运行态
    http_client::apply_proxy(url_opt)?;

    log::info!(
        "[GlobalProxy] [GP-009] Configuration updated: {}",
        url_opt
            .map(http_client::mask_url)
            .unwrap_or_else(|| "direct connection".to_string())
    );

    Ok(())
}

/// 当前生效的出站代理。
pub fn upstream_proxy_status() -> UpstreamProxyStatus {
    let url = http_client::get_current_proxy_url();
    UpstreamProxyStatus {
        enabled: url.is_some(),
        proxy_url: url,
    }
}

/// 测一次代理连通性：用多个测试目标，任一成功即认为可用。
pub async fn test_proxy_url(url: String) -> Result<ProxyTestResult, String> {
    if url.trim().is_empty() {
        return Err("Proxy URL is empty".to_string());
    }

    let start = Instant::now();

    // 构建带代理的临时客户端
    let proxy = reqwest::Proxy::all(&url).map_err(|e| format!("Invalid proxy URL: {e}"))?;

    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build client: {e}"))?;

    // 使用多个测试目标，提高兼容性
    // 优先使用 httpbin（专门用于 HTTP 测试），回退到其他公共端点
    let test_urls = [
        "https://httpbin.org/get",
        "https://www.google.com",
        "https://api.anthropic.com",
    ];

    let mut last_error = None;

    for test_url in test_urls {
        match client.head(test_url).send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis() as u64;
                log::debug!(
                    "[GlobalProxy] Test successful: {} -> {} via {} ({}ms)",
                    http_client::mask_url(&url),
                    test_url,
                    resp.status(),
                    latency
                );
                return Ok(ProxyTestResult {
                    success: true,
                    latency_ms: latency,
                    error: None,
                });
            }
            Err(e) => {
                log::debug!("[GlobalProxy] Test to {test_url} failed: {e}");
                last_error = Some(e);
            }
        }
    }

    // 所有测试目标都失败
    let latency = start.elapsed().as_millis() as u64;
    let error_msg = last_error
        .map(|e| e.to_string())
        .unwrap_or_else(|| "All test targets failed".to_string());

    log::debug!(
        "[GlobalProxy] Test failed: {} -> {} ({}ms)",
        http_client::mask_url(&url),
        error_msg,
        latency
    );

    Ok(ProxyTestResult {
        success: false,
        latency_ms: latency,
        error: Some(error_msg),
    })
}

/// 扫描本机常见代理端口。
///
/// 在阻塞线程池里跑，避免卡住调用方的运行时：这是纯 TCP connect 超时探测。
pub fn scan_local_proxies_blocking() -> Vec<DetectedProxy> {
    let mut found = Vec::new();

    for &(port, primary_type, is_mixed) in PROXY_PORTS {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        if TcpStream::connect_timeout(&addr.into(), Duration::from_millis(100)).is_ok() {
            // 添加主要类型
            found.push(DetectedProxy {
                url: format!("{primary_type}://127.0.0.1:{port}"),
                proxy_type: primary_type.to_string(),
                port,
            });
            // 对于 mixed 端口，同时添加另一种协议
            if is_mixed {
                let alt_type = if primary_type == "http" {
                    "socks5"
                } else {
                    "http"
                };
                found.push(DetectedProxy {
                    url: format!("{alt_type}://127.0.0.1:{port}"),
                    proxy_type: alt_type.to_string(),
                    port,
                });
            }
        }
    }

    found
}
