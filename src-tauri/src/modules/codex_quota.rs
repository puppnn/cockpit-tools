use crate::models::codex::{CodexAccount, CodexQuota, CodexQuotaErrorInfo};
use crate::modules::{codex_account, logger};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;

// 使用 wham/usage 端点（Quotio 使用的）
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const API_KEY_REFRESH_ALL_MIN_INTERVAL_SECONDS: i64 = 15 * 60;
const API_KEY_BILLING_UNLIMITED_TOTAL_THRESHOLD: f64 = 1_000_000.0;

fn get_header_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_string()
}

fn extract_detail_code_from_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;

    if let Some(code) = value
        .get("detail")
        .and_then(|detail| detail.get("code"))
        .and_then(|code| code.as_str())
    {
        return Some(code.to_string());
    }

    if let Some(code) = value
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(|code| code.as_str())
    {
        return Some(code.to_string());
    }

    if let Some(code) = value.get("code").and_then(|code| code.as_str()) {
        return Some(code.to_string());
    }

    None
}

fn extract_error_code_from_message(message: &str) -> Option<String> {
    let marker = "[error_code:";
    let start = message.find(marker)?;
    let code_start = start + marker.len();
    let end = message[code_start..].find(']')?;
    Some(message[code_start..code_start + end].to_string())
}

fn should_force_refresh_token(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("token_invalidated")
        || lower.contains("your authentication token has been invalidated")
        || lower.contains("401 unauthorized")
}

fn write_quota_error(account: &mut CodexAccount, message: String) {
    account.quota_error = Some(CodexQuotaErrorInfo {
        code: extract_error_code_from_message(&message),
        message,
        timestamp: chrono::Utc::now().timestamp(),
    });
}

/// 使用率窗口（5小时/周）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WindowInfo {
    #[serde(rename = "used_percent")]
    used_percent: Option<i32>,
    #[serde(rename = "limit_window_seconds")]
    limit_window_seconds: Option<i64>,
    #[serde(rename = "reset_after_seconds")]
    reset_after_seconds: Option<i64>,
    #[serde(rename = "reset_at")]
    reset_at: Option<i64>,
}

/// 速率限制信息
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RateLimitInfo {
    allowed: Option<bool>,
    #[serde(rename = "limit_reached")]
    limit_reached: Option<bool>,
    #[serde(rename = "primary_window")]
    primary_window: Option<WindowInfo>,
    #[serde(rename = "secondary_window")]
    secondary_window: Option<WindowInfo>,
}

/// 使用率响应
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageResponse {
    #[serde(rename = "plan_type")]
    plan_type: Option<String>,
    #[serde(rename = "rate_limit")]
    rate_limit: Option<RateLimitInfo>,
    #[serde(rename = "code_review_rate_limit")]
    code_review_rate_limit: Option<RateLimitInfo>,
}

fn json_number(value: &Value, key: &str) -> Option<f64> {
    let item = value.get(key)?;
    if let Some(number) = item.as_f64() {
        return Some(number);
    }
    item.as_str()?.trim().parse::<f64>().ok()
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    let item = value.get(key)?;
    if let Some(number) = item.as_i64() {
        return Some(number);
    }
    item.as_str()?.trim().parse::<i64>().ok()
}

fn api_error_message(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    if error.is_null() {
        return None;
    }
    if let Some(message) = error.get("message").and_then(|item| item.as_str()) {
        return Some(message.to_string());
    }
    if let Some(message) = error.as_str() {
        return Some(message.to_string());
    }
    Some(error.to_string())
}

#[derive(Debug, Clone, Default)]
struct ConsoleAuthState {
    bearer_token: Option<String>,
    session_cookie: Option<String>,
    user_id: Option<String>,
}

fn normalize_console_session_cookie(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_prefix = trimmed.strip_prefix("session=").unwrap_or(trimmed);
    let value = without_prefix
        .split(';')
        .next()
        .map(str::trim)
        .filter(|item| !item.is_empty())?;
    Some(value.to_string())
}

fn parse_console_auth(raw: Option<&str>) -> ConsoleAuthState {
    let Some(trimmed) = raw.map(str::trim).filter(|item| !item.is_empty()) else {
        return ConsoleAuthState::default();
    };
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        let bearer_token = value
            .get("token")
            .or_else(|| value.get("access_token"))
            .and_then(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string);
        let session_cookie = value
            .get("session")
            .or_else(|| value.get("session_cookie"))
            .or_else(|| value.get("cookie"))
            .and_then(|item| item.as_str())
            .and_then(normalize_console_session_cookie);
        let user_id = value
            .get("user_id")
            .or_else(|| value.get("id"))
            .and_then(|item| {
                item.as_i64()
                    .map(|value| value.to_string())
                    .or_else(|| item.as_str().map(str::trim).map(str::to_string))
            })
            .filter(|item| !item.is_empty());
        return ConsoleAuthState {
            bearer_token,
            session_cookie,
            user_id,
        };
    }
    ConsoleAuthState {
        bearer_token: Some(
            trimmed
                .strip_prefix("Bearer ")
                .or_else(|| trimmed.strip_prefix("bearer "))
                .unwrap_or(trimmed)
                .trim()
                .to_string(),
        ),
        ..ConsoleAuthState::default()
    }
}

fn build_console_root_candidates(base_url: &str) -> Vec<String> {
    let normalized = base_url.trim().trim_end_matches('/');
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    let mut push_root = |root: String| {
        let root = root.trim().trim_end_matches('/').to_string();
        if !root.is_empty() && seen.insert(root.clone()) {
            roots.push(root);
        }
    };

    let lower = normalized.to_ascii_lowercase();
    if lower.contains("llmskill.cn") {
        push_root("https://www.tokenforus.org".to_string());
        push_root("https://tokenforus.org".to_string());
    }

    if lower.ends_with("/v1") {
        push_root(normalized[..normalized.len() - 3].to_string());
        push_root(normalized.to_string());
    } else {
        push_root(normalized.to_string());
        push_root(format!("{}/v1", normalized));
    }

    roots
}

fn build_api_key_billing_url_candidates(base_url: &str) -> Vec<(String, String)> {
    let normalized = base_url.trim().trim_end_matches('/');
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    let mut push_root = |root: String| {
        let root = root.trim().trim_end_matches('/').to_string();
        if root.is_empty() {
            return;
        }
        let subscription_url = format!("{}/dashboard/billing/subscription", root);
        let usage_url = format!("{}/dashboard/billing/usage", root);
        if seen.insert(subscription_url.clone()) {
            candidates.push((subscription_url, usage_url));
        }
    };

    let lower = normalized.to_ascii_lowercase();
    if lower.ends_with("/v1") {
        push_root(normalized.to_string());
        push_root(normalized[..normalized.len() - 3].to_string());
    } else {
        push_root(format!("{}/v1", normalized));
        push_root(normalized.to_string());
    }

    candidates
}

async fn fetch_api_key_billing_json(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
) -> Result<Value, String> {
    let response = client
        .get(url)
        .bearer_auth(api_key)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {}", e))?;
    let body_len = body.len();

    if !status.is_success() {
        return Err(format!("{} 返回 {} [body_len:{}]", url, status, body_len));
    }

    let value: Value = serde_json::from_str(&body).map_err(|e| format!("解析 JSON 失败: {}", e))?;
    if let Some(message) = api_error_message(&value) {
        return Err(format!("{} 返回错误: {}", url, message));
    }

    Ok(value)
}

async fn fetch_public_json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {}", e))?;
    let body_len = body.len();

    if !status.is_success() {
        return Err(format!("{} 返回 {} [body_len:{}]", url, status, body_len));
    }

    let value: Value = serde_json::from_str(&body).map_err(|e| format!("解析 JSON 失败: {}", e))?;
    if let Some(message) = api_error_message(&value) {
        return Err(format!("{} 返回错误: {}", url, message));
    }

    Ok(value)
}

#[derive(Debug, Clone)]
struct JsonFetchResult {
    value: Value,
    final_url: reqwest::Url,
}

async fn fetch_public_json_with_meta(
    client: &reqwest::Client,
    url: &str,
) -> Result<JsonFetchResult, String> {
    let response = client
        .get(url)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("璇锋眰澶辫触: {}", e))?;

    let final_url = response.url().clone();
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("璇诲彇鍝嶅簲澶辫触: {}", e))?;
    let body_len = body.len();

    if !status.is_success() {
        return Err(format!("{} 杩斿洖 {} [body_len:{}]", url, status, body_len));
    }

    let value: Value = serde_json::from_str(&body).map_err(|e| format!("瑙ｆ瀽 JSON 澶辫触: {}", e))?;
    if let Some(message) = api_error_message(&value) {
        return Err(format!("{} 杩斿洖閿欒: {}", url, message));
    }

    Ok(JsonFetchResult { value, final_url })
}

async fn fetch_console_session_json(
    client: &reqwest::Client,
    url: &str,
    session_cookie: &str,
    user_id: &str,
) -> Result<Value, String> {
    let response = client
        .get(url)
        .header(ACCEPT, "application/json")
        .header("Cookie", format!("session={}", session_cookie))
        .header("New-API-User", user_id)
        .header("Cache-Control", "no-store")
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {}", e))?;
    let body_len = body.len();

    if !status.is_success() {
        return Err(format!("{} 返回 {} [body_len:{}]", url, status, body_len));
    }

    let value: Value = serde_json::from_str(&body).map_err(|e| format!("解析 JSON 失败: {}", e))?;
    if let Some(message) = api_error_message(&value) {
        return Err(format!("{} 返回错误: {}", url, message));
    }

    Ok(value)
}

fn derive_console_root_from_endpoint_url(final_url: &reqwest::Url, endpoint_path: &str) -> Option<String> {
    let path = final_url.path();
    let base_path = path.strip_suffix(endpoint_path)?.trim_end_matches('/');
    let host = final_url.host_str()?;

    let mut root = format!("{}://{}", final_url.scheme(), host);
    if let Some(port) = final_url.port() {
        root.push(':');
        root.push_str(&port.to_string());
    }
    if !base_path.is_empty() {
        root.push_str(base_path);
    }

    Some(root.trim_end_matches('/').to_string())
}

fn quota_status_scale(status: &Value) -> (f64, String) {
    let quota_per_unit = json_number(status, "quota_per_unit")
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(500_000.0);
    let display_type = status
        .get("quota_display_type")
        .and_then(|item| item.as_str())
        .unwrap_or("USD")
        .to_ascii_uppercase();
    let rate = match display_type.as_str() {
        "CNY" => json_number(status, "usd_exchange_rate").unwrap_or(1.0),
        "CUSTOM" => json_number(status, "custom_currency_exchange_rate").unwrap_or(1.0),
        _ => 1.0,
    };
    let symbol = match display_type.as_str() {
        "CNY" => "¥".to_string(),
        "CUSTOM" => status
            .get("custom_currency_symbol")
            .and_then(|item| item.as_str())
            .unwrap_or("¤")
            .to_string(),
        "TOKENS" => String::new(),
        _ => "$".to_string(),
    };
    (rate / quota_per_unit, symbol)
}

fn json_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn json_number_path(value: &Value, path: &[&str]) -> Option<f64> {
    let item = json_path(value, path)?;
    if let Some(number) = item.as_f64() {
        return Some(number);
    }
    item.as_str()?.trim().parse::<f64>().ok()
}

fn json_string_path(value: &Value, path: &[&str]) -> Option<String> {
    json_path(value, path)?
        .as_str()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
}

fn subscription_is_active(item: &Value, now: i64) -> bool {
    let status = json_string_path(item, &["subscription", "status"])
        .or_else(|| json_string_path(item, &["status"]))
        .unwrap_or_default();
    let end_time = json_number_path(item, &["subscription", "end_time"])
        .or_else(|| json_number_path(item, &["end_time"]))
        .unwrap_or(0.0) as i64;
    status == "active" && (end_time <= 0 || end_time > now)
}

fn build_quota_from_subscription_console(
    base_url: &str,
    subscription_url: &str,
    status: Value,
    subscription_payload: Value,
) -> Result<CodexQuota, String> {
    let data = subscription_payload
        .get("data")
        .ok_or_else(|| "订阅接口缺少 data 字段".to_string())?;
    let subscriptions = data
        .get("subscriptions")
        .and_then(|item| item.as_array())
        .or_else(|| {
            data.get("all_subscriptions")
                .and_then(|item| item.as_array())
        })
        .ok_or_else(|| "订阅接口没有返回套餐列表".to_string())?;
    let now = chrono::Utc::now().timestamp();
    let active_items: Vec<&Value> = subscriptions
        .iter()
        .filter(|item| subscription_is_active(item, now))
        .collect();
    let selected_items: Vec<&Value> = if active_items.is_empty() {
        subscriptions.iter().collect()
    } else {
        active_items
    };

    let mut total_quota = 0.0;
    let mut used_quota = 0.0;
    let mut title = None;
    let mut access_until = None;

    for item in selected_items {
        total_quota += json_number_path(item, &["subscription", "amount_total"])
            .or_else(|| json_number_path(item, &["amount_total"]))
            .unwrap_or(0.0);
        used_quota += json_number_path(item, &["subscription", "amount_used"])
            .or_else(|| json_number_path(item, &["amount_used"]))
            .unwrap_or(0.0);
        if title.is_none() {
            title = json_string_path(item, &["plan", "title"])
                .or_else(|| json_string_path(item, &["subscription", "plan", "title"]));
        }
        if access_until.is_none() {
            access_until = json_number_path(item, &["subscription", "end_time"])
                .or_else(|| json_number_path(item, &["end_time"]))
                .map(|value| value as i64)
                .filter(|value| *value > 0);
        }
    }

    if total_quota <= 0.0 {
        return Err("订阅接口没有返回有效总额度".to_string());
    }

    let (scale, symbol) = quota_status_scale(&status);
    let total_amount = total_quota * scale;
    let used_amount = used_quota.max(0.0) * scale;
    let remaining_amount = (total_amount - used_amount).max(0.0);
    let remaining_percentage = if total_amount > 0.0 {
        ((remaining_amount / total_amount) * 100.0)
            .round()
            .clamp(0.0, 100.0) as i32
    } else {
        100
    };

    Ok(CodexQuota {
        hourly_percentage: remaining_percentage,
        hourly_reset_time: None,
        hourly_window_minutes: None,
        hourly_window_present: Some(false),
        weekly_percentage: remaining_percentage,
        weekly_reset_time: access_until,
        weekly_window_minutes: None,
        weekly_window_present: Some(false),
        raw_data: Some(json!({
            "api_key_billing": {
                "source": "subscription_console",
                "base_url": base_url,
                "subscription_url": subscription_url,
                "remaining_amount": remaining_amount,
                "used_amount": used_amount,
                "total_amount": total_amount,
                "remaining_percentage": remaining_percentage,
                "access_until": access_until,
                "currency_symbol": symbol,
                "title": title,
                "status": status,
                "subscription": subscription_payload,
            }
        })),
    })
}

async fn fetch_console_subscription_quota(
    account: &CodexAccount,
) -> Result<Option<CodexQuota>, String> {
    let auth = parse_console_auth(account.api_console_token.as_deref());
    if auth.bearer_token.is_none() && (auth.session_cookie.is_none() || auth.user_id.is_none()) {
        return Ok(None);
    }
    let base_url = account
        .api_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "控制台额度需要第三方 Base URL".to_string())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;

    let mut errors = Vec::new();
    for root in build_console_root_candidates(base_url) {
        let status_url = format!("{}/api/status", root);
        let default_subscription_url = format!("{}/api/subscription/self", root);
        let status_result = match fetch_public_json_with_meta(&client, &status_url).await {
            Ok(value) => value,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let status = status_result
            .value
            .get("data")
            .cloned()
            .unwrap_or(status_result.value);
        let subscription_url = derive_console_root_from_endpoint_url(
            &status_result.final_url,
            "/api/status",
        )
        .map(|resolved_root| format!("{}/api/subscription/self", resolved_root))
        .unwrap_or(default_subscription_url);

        if let (Some(session_cookie), Some(user_id)) =
            (auth.session_cookie.as_deref(), auth.user_id.as_deref())
        {
            match fetch_console_session_json(&client, &subscription_url, session_cookie, user_id)
                .await
            {
                Ok(subscription)
                    if subscription
                        .get("success")
                        .and_then(|item| item.as_bool())
                        .unwrap_or(false) =>
                {
                    return build_quota_from_subscription_console(
                        base_url,
                        &subscription_url,
                        status.clone(),
                        subscription,
                    )
                    .map(Some);
                }
                Ok(subscription) => {
                    if let Some(message) =
                        subscription.get("message").and_then(|item| item.as_str())
                    {
                        errors.push(format!("{} 返回错误: {}", subscription_url, message));
                    }
                }
                Err(error) => errors.push(error),
            }
        }

        if let Some(token) = auth.bearer_token.as_deref() {
            match fetch_api_key_billing_json(&client, &subscription_url, token).await {
                Ok(subscription)
                    if subscription
                        .get("success")
                        .and_then(|item| item.as_bool())
                        .unwrap_or(false) =>
                {
                    return build_quota_from_subscription_console(
                        base_url,
                        &subscription_url,
                        status,
                        subscription,
                    )
                    .map(Some);
                }
                Ok(subscription) => {
                    if let Some(message) =
                        subscription.get("message").and_then(|item| item.as_str())
                    {
                        errors.push(format!("{} 返回错误: {}", subscription_url, message));
                    }
                }
                Err(error) => errors.push(error),
            }
        }
    }

    let detail = if errors.is_empty() {
        "没有可用的订阅接口".to_string()
    } else {
        errors.join("; ")
    };
    Err(format!("第三方控制台订阅额度读取失败: {}", detail))
}

fn build_quota_from_api_key_billing(
    base_url: &str,
    subscription_url: &str,
    usage_url: &str,
    subscription: Value,
    usage: Value,
) -> Result<CodexQuota, String> {
    let used_amount = json_number(&usage, "total_usage").unwrap_or(0.0) / 100.0;
    let total_amount = [
        json_number(&subscription, "hard_limit_usd"),
        json_number(&subscription, "system_hard_limit_usd"),
        json_number(&subscription, "soft_limit_usd"),
    ]
    .into_iter()
    .flatten()
    .filter(|value| value.is_finite() && *value > 0.0)
    .fold(0.0_f64, f64::max);

    let total_amount = if total_amount > 0.0 {
        total_amount
    } else if used_amount > 0.0 {
        used_amount
    } else {
        0.0
    };
    if total_amount >= API_KEY_BILLING_UNLIMITED_TOTAL_THRESHOLD {
        return Err("第三方 API Key 接口返回的是无限 Key 额度，不是真实订阅套餐额度；请配置控制台登录信息或 Token 后刷新。".to_string());
    }
    let remaining_amount = (total_amount - used_amount).max(0.0);
    let remaining_percentage = if total_amount > 0.0 {
        ((remaining_amount / total_amount) * 100.0)
            .round()
            .clamp(0.0, 100.0) as i32
    } else {
        100
    };
    let access_until = json_i64(&subscription, "access_until");

    Ok(CodexQuota {
        hourly_percentage: remaining_percentage,
        hourly_reset_time: None,
        hourly_window_minutes: None,
        hourly_window_present: Some(false),
        weekly_percentage: remaining_percentage,
        weekly_reset_time: access_until.filter(|value| *value > 0),
        weekly_window_minutes: None,
        weekly_window_present: Some(false),
        raw_data: Some(json!({
            "api_key_billing": {
                "source": "openai_compatible_billing",
                "base_url": base_url,
                "subscription_url": subscription_url,
                "usage_url": usage_url,
                "remaining_amount": remaining_amount,
                "used_amount": used_amount.max(0.0),
                "total_amount": total_amount,
                "remaining_percentage": remaining_percentage,
                "access_until": access_until,
                "subscription": subscription,
                "usage": usage,
            }
        })),
    })
}

async fn fetch_api_key_quota(account: &CodexAccount) -> Result<CodexQuota, String> {
    if let Some(quota) = fetch_console_subscription_quota(account).await? {
        return Ok(quota);
    }

    let api_key = account
        .openai_api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "API Key 账号缺少 API Key".to_string())?;
    let base_url = account
        .api_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "API Key 账号缺少第三方 Base URL，无法读取第三方额度".to_string())?;

    let candidates = build_api_key_billing_url_candidates(base_url);
    if candidates.is_empty() {
        return Err("第三方 Base URL 为空，无法读取额度".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;
    let mut errors = Vec::new();

    for (subscription_url, usage_url) in candidates {
        logger::log_info(&format!(
            "Codex API Key 额度请求: subscription={}, usage={}",
            subscription_url, usage_url
        ));

        let subscription =
            match fetch_api_key_billing_json(&client, &subscription_url, api_key).await {
                Ok(value) => value,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };
        let usage = match fetch_api_key_billing_json(&client, &usage_url, api_key).await {
            Ok(value) => value,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };

        return build_quota_from_api_key_billing(
            base_url,
            &subscription_url,
            &usage_url,
            subscription,
            usage,
        );
    }

    let detail = if errors.is_empty() {
        "没有可用的额度接口".to_string()
    } else {
        errors.join("; ")
    };
    Err(format!("第三方 API Key 额度接口不可用: {}", detail))
}

fn normalize_remaining_percentage(window: &WindowInfo) -> i32 {
    let used = window.used_percent.unwrap_or(0).clamp(0, 100);
    100 - used
}

fn normalize_window_minutes(window: &WindowInfo) -> Option<i64> {
    let seconds = window.limit_window_seconds?;
    if seconds <= 0 {
        return None;
    }
    Some((seconds + 59) / 60)
}

fn normalize_reset_time(window: &WindowInfo) -> Option<i64> {
    if let Some(reset_at) = window.reset_at {
        return Some(reset_at);
    }

    let reset_after_seconds = window.reset_after_seconds?;
    if reset_after_seconds < 0 {
        return None;
    }

    Some(chrono::Utc::now().timestamp() + reset_after_seconds)
}

/// 配额查询结果（包含 plan_type）
pub struct FetchQuotaResult {
    pub quota: CodexQuota,
    pub plan_type: Option<String>,
}

async fn refresh_account_tokens(account: &mut CodexAccount, reason: &str) -> Result<(), String> {
    let refresh_token = account
        .tokens
        .refresh_token
        .clone()
        .ok_or_else(|| format!("{}，且账号缺少 refresh_token", reason))?;

    logger::log_info(&format!(
        "Codex 账号 {} 触发强制 Token 刷新: {}",
        account.email, reason
    ));

    let new_tokens = crate::modules::codex_oauth::refresh_access_token_with_fallback(
        &refresh_token,
        Some(account.tokens.id_token.as_str()),
    )
    .await
    .map_err(|e| format!("{}，刷新 Token 失败: {}", reason, e))?;

    account.tokens = new_tokens;
    Ok(())
}

/// 查询单个账号的配额
pub async fn fetch_quota(account: &CodexAccount) -> Result<FetchQuotaResult, String> {
    let client = reqwest::Client::new();

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", account.tokens.access_token))
            .map_err(|e| format!("构建 Authorization 头失败: {}", e))?,
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

    // 添加 ChatGPT-Account-Id 头（关键！）
    let account_id = account.account_id.clone().or_else(|| {
        codex_account::extract_chatgpt_account_id_from_access_token(&account.tokens.access_token)
    });

    if let Some(ref acc_id) = account_id {
        if !acc_id.is_empty() {
            headers.insert(
                "ChatGPT-Account-Id",
                HeaderValue::from_str(acc_id)
                    .map_err(|e| format!("构建 Account-Id 头失败: {}", e))?,
            );
        }
    }

    logger::log_info(&format!(
        "Codex 配额请求: {} (account_id: {:?})",
        USAGE_URL, account_id
    ));

    let response = client
        .get(USAGE_URL)
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;

    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {}", e))?;

    let request_id = get_header_value(&headers, "request-id");
    let x_request_id = get_header_value(&headers, "x-request-id");
    let cf_ray = get_header_value(&headers, "cf-ray");
    let body_len = body.len();

    logger::log_info(&format!(
        "Codex 配额响应元信息: url={}, status={}, request-id={}, x-request-id={}, cf-ray={}, body_len={}",
        USAGE_URL, status, request_id, x_request_id, cf_ray, body_len
    ));

    if !status.is_success() {
        let detail_code = extract_detail_code_from_body(&body);

        logger::log_error(&format!(
            "Codex 配额接口返回非成功状态: url={}, status={}, request-id={}, x-request-id={}, cf-ray={}, detail_code={:?}, body_len={}",
            USAGE_URL,
            status,
            request_id,
            x_request_id,
            cf_ray,
            detail_code,
            body_len
        ));

        let mut error_message = format!("API 返回错误 {}", status);
        if let Some(code) = detail_code {
            error_message.push_str(&format!(" [error_code:{}]", code));
        }
        error_message.push_str(&format!(" [body_len:{}]", body_len));
        return Err(error_message);
    }

    // 解析响应
    let usage: UsageResponse =
        serde_json::from_str(&body).map_err(|e| format!("解析 JSON 失败: {}", e))?;

    let quota = parse_quota_from_usage(&usage, &body)?;
    let plan_type = usage.plan_type.clone();

    Ok(FetchQuotaResult { quota, plan_type })
}

/// 从使用率响应中解析配额信息
fn parse_quota_from_usage(usage: &UsageResponse, raw_body: &str) -> Result<CodexQuota, String> {
    let rate_limit = usage.rate_limit.as_ref();
    let primary_window = rate_limit.and_then(|r| r.primary_window.as_ref());
    let secondary_window = rate_limit.and_then(|r| r.secondary_window.as_ref());

    // Primary window = 5小时配额（session）
    let (hourly_percentage, hourly_reset_time, hourly_window_minutes) =
        if let Some(primary) = primary_window {
            (
                normalize_remaining_percentage(primary),
                normalize_reset_time(primary),
                normalize_window_minutes(primary),
            )
        } else {
            (100, None, None)
        };

    // Secondary window = 周配额
    let (weekly_percentage, weekly_reset_time, weekly_window_minutes) =
        if let Some(secondary) = secondary_window {
            (
                normalize_remaining_percentage(secondary),
                normalize_reset_time(secondary),
                normalize_window_minutes(secondary),
            )
        } else {
            (100, None, None)
        };

    // 保存原始响应
    let raw_data: Option<serde_json::Value> = serde_json::from_str(raw_body).ok();

    Ok(CodexQuota {
        hourly_percentage,
        hourly_reset_time,
        hourly_window_minutes,
        hourly_window_present: Some(primary_window.is_some()),
        weekly_percentage,
        weekly_reset_time,
        weekly_window_minutes,
        weekly_window_present: Some(secondary_window.is_some()),
        raw_data,
    })
}

/// 从 id_token 中提取 plan_type 并同步更新账号和索引
fn sync_plan_type_from_token(account: &mut CodexAccount, plan_type: Option<String>) {
    if let Some(ref new_plan) = plan_type {
        let old_plan = account.plan_type.clone();
        if account.plan_type.as_deref() != Some(new_plan) {
            logger::log_info(&format!(
                "Codex 账号 {} 订阅标识已更新: {:?} -> {:?}",
                account.email, old_plan, plan_type
            ));
            account.plan_type = plan_type;
            // 同步更新索引中的 plan_type
            if let Err(e) =
                codex_account::update_account_plan_type_in_index(&account.id, &account.plan_type)
            {
                logger::log_warn(&format!("更新索引 plan_type 失败: {}", e));
            }
        }
    }
}

/// 刷新账号配额并保存（包含 token 自动刷新）
async fn refresh_api_key_account_quota(account: &mut CodexAccount) -> Result<CodexQuota, String> {
    match fetch_api_key_quota(account).await {
        Ok(quota) => {
            account.quota = Some(quota.clone());
            account.quota_error = None;
            account.usage_updated_at = Some(chrono::Utc::now().timestamp());
            codex_account::save_account(account)?;
            Ok(quota)
        }
        Err(e) => {
            write_quota_error(account, e.clone());
            if e.contains("无限 Key 额度") {
                account.quota = None;
            }
            account.usage_updated_at = Some(chrono::Utc::now().timestamp());
            if let Err(save_err) = codex_account::save_account(account) {
                logger::log_warn(&format!("鍐欏叆 Codex 閰嶉閿欒澶辫触: {}", save_err));
            }
            Err(e)
        }
    }
}

async fn refresh_account_quota_once(account_id: &str) -> Result<CodexQuota, String> {
    let mut account = codex_account::load_account(account_id)
        .ok_or_else(|| format!("账号不存在: {}", account_id))?;
    if account.is_api_key_auth() {
        return refresh_api_key_account_quota(&mut account).await;
    }

    // 检查 token 是否过期，如果过期则刷新
    if crate::modules::codex_oauth::is_token_expired(&account.tokens.access_token) {
        match refresh_account_tokens(&mut account, "Token 已过期").await {
            Ok(()) => {
                logger::log_info(&format!("账号 {} 的 Token 刷新成功", account.email));

                // 从新的 id_token 重新解析 plan_type
                if let Ok((_, _, new_plan_type, _, _)) =
                    codex_account::extract_user_info(&account.tokens.id_token)
                {
                    sync_plan_type_from_token(&mut account, new_plan_type);
                }

                codex_account::save_account(&account)?;
            }
            Err(e) => {
                logger::log_error(&format!("账号 {} Token 刷新失败: {}", account.email, e));
                let message = e;
                write_quota_error(&mut account, message.clone());
                if let Err(save_err) = codex_account::save_account(&account) {
                    logger::log_warn(&format!("写入 Codex 配额错误失败: {}", save_err));
                }
                return Err(message);
            }
        }
    }

    let result = match fetch_quota(&account).await {
        Ok(result) => result,
        Err(e) if should_force_refresh_token(&e) => {
            logger::log_warn(&format!(
                "Codex 配额请求检测到失效 Token，准备强制刷新后重试: account={}, error={}",
                account.email, e
            ));

            match refresh_account_tokens(&mut account, "配额接口返回 Token 失效").await {
                Ok(()) => {
                    if let Ok((_, _, new_plan_type, _, _)) =
                        codex_account::extract_user_info(&account.tokens.id_token)
                    {
                        sync_plan_type_from_token(&mut account, new_plan_type);
                    }
                    codex_account::save_account(&account)?;

                    match fetch_quota(&account).await {
                        Ok(result) => result,
                        Err(retry_err) => {
                            write_quota_error(&mut account, retry_err.clone());
                            if let Err(save_err) = codex_account::save_account(&account) {
                                logger::log_warn(&format!("写入 Codex 配额错误失败: {}", save_err));
                            }
                            return Err(retry_err);
                        }
                    }
                }
                Err(refresh_err) => {
                    write_quota_error(&mut account, refresh_err.clone());
                    if let Err(save_err) = codex_account::save_account(&account) {
                        logger::log_warn(&format!("写入 Codex 配额错误失败: {}", save_err));
                    }
                    return Err(refresh_err);
                }
            }
        }
        Err(e) => {
            write_quota_error(&mut account, e.clone());
            if let Err(save_err) = codex_account::save_account(&account) {
                logger::log_warn(&format!("写入 Codex 配额错误失败: {}", save_err));
            }
            return Err(e);
        }
    };

    // 从 usage 响应中的 plan_type 更新订阅标识
    if result.plan_type.is_some() {
        sync_plan_type_from_token(&mut account, result.plan_type);
    }

    account.quota = Some(result.quota.clone());
    account.quota_error = None;
    account.usage_updated_at = Some(chrono::Utc::now().timestamp());
    codex_account::save_account(&account)?;

    Ok(result.quota)
}

pub async fn refresh_account_quota(account_id: &str) -> Result<CodexQuota, String> {
    crate::modules::refresh_retry::retry_once_with_delay("Codex Refresh", account_id, || async {
        refresh_account_quota_once(account_id).await
    })
    .await
}

/// 刷新所有账号配额
fn should_refresh_account_in_batch(account: &CodexAccount, now: i64) -> bool {
    if !account.is_api_key_auth() {
        return true;
    }
    if account
        .api_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        return false;
    }
    match account.usage_updated_at {
        Some(updated_at) => {
            now.saturating_sub(updated_at) >= API_KEY_REFRESH_ALL_MIN_INTERVAL_SECONDS
        }
        None => true,
    }
}

pub async fn refresh_all_quotas() -> Result<Vec<(String, Result<CodexQuota, String>)>, String> {
    use futures::future::join_all;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    const MAX_CONCURRENT: usize = 5;
    let now = chrono::Utc::now().timestamp();
    let accounts: Vec<_> = codex_account::list_accounts()
        .into_iter()
        .filter(|account| should_refresh_account_in_batch(account, now))
        .collect();

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let tasks: Vec<_> = accounts
        .into_iter()
        .map(|account| {
            let account_id = account.id;
            let semaphore = semaphore.clone();
            async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|e| format!("获取 Codex 刷新并发许可失败: {}", e))?;
                let result = refresh_account_quota(&account_id).await;
                Ok::<(String, Result<CodexQuota, String>), String>((account_id, result))
            }
        })
        .collect();

    let mut results = Vec::with_capacity(tasks.len());
    for task in join_all(tasks).await {
        match task {
            Ok(item) => results.push(item),
            Err(err) => return Err(err),
        }
    }

    Ok(results)
}
