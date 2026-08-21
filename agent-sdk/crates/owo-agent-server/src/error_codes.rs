//! 分层错误码（R6 Agent 4 Wave 1）：`域/原因/可恢复性` 契约。
//!
//! 错误码形如 `gateway/rate_limited/retryable`：
//! - 域（domain）：gateway / permission / auth / validation / storage / tool / internal；
//! - 原因（reason）：rate_limited / unavailable / timeout / denied / …；
//! - 可恢复性（retryable）：retryable / not_retryable。
//!
//! [`ErrorCode::from_code`] 解析并映射 HTTP 状态与 `retry_after` 语义；
//! [`ErrorCode::lookup`] 按（域, 原因）查已知注册表。
//! 本模块不引用 `crate::`/`super::`，可被测试以 `#[path] mod` 独立编译。

// 主控收尾接线说明：lib 目标当前仅登记模块（无路由引用），全部符号由
// error_codes_tests 以 #[path] 独立编译使用；与 team_api.rs 同款模块级
// allow(dead_code)（后续接入错误面后移除）。
#![allow(dead_code)]

use serde_json::{json, Value};
use std::time::Duration;

/// 分层错误码：域/原因/可恢复性 + HTTP 映射 + 重试语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorCode {
    pub domain: String,
    pub reason: String,
    pub retryable: bool,
    pub http_status: u16,
    pub retry_after_ms: Option<u64>,
}

impl ErrorCode {
    /// 解析 `domain/reason/retryable` 形式错误码（严格三段式）。
    pub fn from_code(code: &str) -> Result<ErrorCode, String> {
        let mut parts = code.split('/');
        let domain = parts.next().unwrap_or("").trim();
        let reason = parts.next().unwrap_or("").trim();
        let retryable = parts.next().unwrap_or("").trim();
        if domain.is_empty() || reason.is_empty() || retryable.is_empty() || parts.next().is_some()
        {
            return Err(format!(
                "非法错误码（需为 domain/reason/retryable 三段式）: {code}"
            ));
        }
        let retryable = match retryable {
            "retryable" => true,
            "not_retryable" => false,
            other => {
                return Err(format!(
                    "非法可恢复性标记（retryable/not_retryable）: {other}"
                ))
            }
        };
        let mut code = Self::lookup(domain, reason).unwrap_or_else(|| {
            // 注册表未命中：按 reason 兜底映射，可恢复性以显式标记为准。
            let (status, retry_after_ms) = Self::fallback_mapping(reason, retryable);
            ErrorCode {
                domain: domain.to_string(),
                reason: reason.to_string(),
                retryable,
                http_status: status,
                retry_after_ms,
            }
        });
        code.retryable = retryable;
        Ok(code)
    }

    /// 按（域, 原因）查已知注册表。
    pub fn lookup(domain: &str, reason: &str) -> Option<ErrorCode> {
        let (status, retry_after_ms, retryable) = known_table().get(domain)?.get(reason)?;
        Some(ErrorCode {
            domain: domain.to_string(),
            reason: reason.to_string(),
            retryable: *retryable,
            http_status: *status,
            retry_after_ms: *retry_after_ms,
        })
    }

    /// HTTP 状态码。
    pub fn http_status(&self) -> u16 {
        self.http_status
    }

    /// 重试等待时长（仅 retryable 且注册表给出时）。
    pub fn retry_after(&self) -> Option<Duration> {
        if !self.retryable {
            return None;
        }
        self.retry_after_ms.map(Duration::from_millis)
    }

    /// 序列化为统一错误响应体（HTTP 面契约）。
    pub fn to_json(&self, message: &str) -> Value {
        json!({
            "code": format!("{}/{}/{}", self.domain, self.reason, if self.retryable { "retryable" } else { "not_retryable" }),
            "domain": self.domain,
            "reason": self.reason,
            "retryable": self.retryable,
            "http_status": self.http_status,
            "retry_after_ms": self.retry_after_ms,
            "message": message,
        })
    }

    fn fallback_mapping(reason: &str, retryable: bool) -> (u16, Option<u64>) {
        match reason {
            "rate_limited" => (429, Some(1_000)),
            "unavailable" => (503, Some(5_000)),
            "timeout" => (504, Some(2_000)),
            "denied" => (403, None),
            "missing_credentials" => (401, None),
            "invalid_input" => (400, None),
            "conflict" => (409, None),
            "not_found" => (404, None),
            "execution_failed" => (502, Some(3_000)),
            "circuit_open" => (503, Some(30_000)),
            _ if retryable => (500, Some(5_000)),
            _ => (500, None),
        }
    }
}

/// 已知错误码注册表：domain → reason → (http_status, retry_after_ms, retryable)。
type ReasonRow = (u16, Option<u64>, bool);
type ReasonMap = std::collections::HashMap<&'static str, ReasonRow>;
type DomainMap = std::collections::HashMap<&'static str, ReasonMap>;

fn known_table() -> &'static DomainMap {
    static TABLE: std::sync::OnceLock<DomainMap> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        use std::collections::HashMap;
        let mut table: DomainMap = HashMap::new();
        let mut insert = |domain: &'static str,
                          reason: &'static str,
                          status: u16,
                          retry_after_ms: Option<u64>,
                          retryable: bool| {
            table
                .entry(domain)
                .or_default()
                .insert(reason, (status, retry_after_ms, retryable));
        };
        insert("gateway", "rate_limited", 429, Some(1_000), true);
        insert("gateway", "unavailable", 503, Some(5_000), true);
        insert("gateway", "timeout", 504, Some(2_000), true);
        insert("gateway", "circuit_open", 503, Some(30_000), true);
        insert("permission", "denied", 403, None, false);
        insert("auth", "missing_credentials", 401, None, false);
        insert("validation", "invalid_input", 400, None, false);
        insert("validation", "conflict", 409, None, false);
        insert("storage", "not_found", 404, None, false);
        insert("tool", "not_found", 404, None, false);
        insert("tool", "execution_failed", 502, Some(3_000), true);
        insert("internal", "unexpected", 500, Some(5_000), true);
        table
    })
}

/// 常见错误码快捷构造（供路由层直接使用）。
pub fn code(domain: &str, reason: &str, retryable: bool) -> ErrorCode {
    ErrorCode::from_code(&format!(
        "{domain}/{reason}/{}",
        if retryable {
            "retryable"
        } else {
            "not_retryable"
        }
    ))
    .unwrap_or_else(|_| ErrorCode {
        domain: domain.to_string(),
        reason: reason.to_string(),
        retryable,
        http_status: 500,
        retry_after_ms: retryable.then_some(5_000),
    })
}
