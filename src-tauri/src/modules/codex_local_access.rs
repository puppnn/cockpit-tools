use crate::models::codex::{CodexAccount, CodexApiProviderMode};
use crate::models::codex_local_access::{
    CodexLocalAccessAccountStats, CodexLocalAccessCollection, CodexLocalAccessRoutingStrategy,
    CodexLocalAccessState, CodexLocalAccessStats, CodexLocalAccessStatsWindow,
    CodexLocalAccessUsageEvent, CodexLocalAccessUsageStats,
};
use crate::modules::atomic_write::write_string_atomic;
use crate::modules::{codex_account, codex_oauth, logger};
use futures_util::StreamExt;
use rand::{distributions::Alphanumeric, Rng};
use reqwest::header::{HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::TcpListener as StdTcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex as TokioMutex};
use tokio::time::{timeout, Duration};

const CODEX_LOCAL_ACCESS_FILE: &str = "codex_local_access.json";
const CODEX_LOCAL_ACCESS_STATS_FILE: &str = "codex_local_access_stats.json";
const MAX_HTTP_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REQUEST_RETRY_WAIT: Duration = Duration::from_secs(3);
const MAX_REQUEST_RETRY_ATTEMPTS: usize = 1;
const MAX_RETRY_CREDENTIALS_PER_REQUEST: usize = 8;
const RESPONSE_AFFINITY_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_RESPONSE_AFFINITY_BINDINGS: usize = 4096;
const DAY_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
const WEEK_WINDOW_MS: i64 = 7 * DAY_WINDOW_MS;
const MONTH_WINDOW_MS: i64 = 30 * DAY_WINDOW_MS;
const MAX_RECENT_USAGE_EVENTS: usize = 5_000;
const UPSTREAM_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const DEFAULT_CODEX_USER_AGENT: &str =
    "codex-tui/0.118.0 (Mac OS 26.3.1; arm64) iTerm.app/3.6.9 (codex-tui; 0.118.0)";
const DEFAULT_CODEX_ORIGINATOR: &str = "codex-tui";
const CORS_ALLOW_HEADERS: &str = "Authorization, Content-Type, OpenAI-Beta, X-API-Key, X-Codex-Beta-Features, X-Client-Request-Id, Originator, Session_id, ChatGPT-Account-Id";
const DEFAULT_CODEX_MODELS: &[&str] = &[
    "gpt-5-codex",
    "gpt-5-codex-mini",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.2",
    "gpt-5.2-codex",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
];

static GATEWAY_RUNTIME: OnceLock<TokioMutex<GatewayRuntime>> = OnceLock::new();
static GATEWAY_ROUND_ROBIN_CURSOR: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct GatewayRuntime {
    loaded: bool,
    collection: Option<CodexLocalAccessCollection>,
    stats: CodexLocalAccessStats,
    response_affinity: HashMap<String, ResponseAffinityBinding>,
    model_cooldowns: HashMap<String, AccountModelCooldown>,
    running: bool,
    actual_port: Option<u16>,
    last_error: Option<String>,
    shutdown_sender: Option<watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, Clone, Default)]
struct UsageCapture {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cached_tokens: u64,
    reasoning_tokens: u64,
}

#[derive(Debug, Clone, Default)]
struct ResponseCapture {
    usage: Option<UsageCapture>,
    response_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ResponseAffinityBinding {
    account_id: String,
    updated_at_ms: i64,
}

#[derive(Debug, Clone)]
struct AccountModelCooldown {
    next_retry_at_ms: i64,
}

#[derive(Debug)]
struct ProxyDispatchSuccess {
    upstream: reqwest::Response,
    account_id: String,
    account_email: String,
}

#[derive(Debug)]
struct ProxyDispatchError {
    status: u16,
    message: String,
    account_id: Option<String>,
    account_email: Option<String>,
}

struct ResponseUsageCollector {
    is_stream: bool,
    body: Vec<u8>,
    stream_buffer: Vec<u8>,
    usage: Option<UsageCapture>,
    response_id: Option<String>,
}

#[derive(Debug)]
struct ParsedRequest {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct RequestRoutingHint {
    model_key: String,
    previous_response_id: Option<String>,
}

#[derive(Debug, Clone)]
struct RoutingCandidate {
    account_id: String,
    plan_rank: Option<i32>,
    remaining_quota: Option<i32>,
}

fn gateway_runtime() -> &'static TokioMutex<GatewayRuntime> {
    GATEWAY_RUNTIME.get_or_init(|| TokioMutex::new(GatewayRuntime::default()))
}

fn local_access_file_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    Ok(home
        .join(".antigravity_cockpit")
        .join(CODEX_LOCAL_ACCESS_FILE))
}

fn local_access_stats_file_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    Ok(home
        .join(".antigravity_cockpit")
        .join(CODEX_LOCAL_ACCESS_STATS_FILE))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn normalize_model_key(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

fn parse_request_body_json(body: &[u8]) -> Option<Value> {
    if body.is_empty() {
        return None;
    }
    serde_json::from_slice::<Value>(body).ok()
}

fn build_request_routing_hint(request: &ParsedRequest) -> RequestRoutingHint {
    let Some(body) = parse_request_body_json(&request.body) else {
        return RequestRoutingHint::default();
    };

    RequestRoutingHint {
        model_key: body
            .get("model")
            .and_then(Value::as_str)
            .map(normalize_model_key)
            .unwrap_or_default(),
        previous_response_id: body
            .get("previous_response_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    }
}

fn build_cooldown_key(account_id: &str, model_key: &str) -> Option<String> {
    let account_id = account_id.trim();
    let model_key = model_key.trim();
    if account_id.is_empty() || model_key.is_empty() {
        return None;
    }
    Some(format!("{}\u{1f}{}", account_id, model_key))
}

fn build_ordered_account_ids(
    account_ids: &[String],
    start: usize,
    preferred_account_id: Option<&str>,
) -> Vec<String> {
    if account_ids.is_empty() {
        return Vec::new();
    }

    let mut ordered = Vec::with_capacity(account_ids.len());
    if let Some(preferred) = preferred_account_id {
        if account_ids.iter().any(|account_id| account_id == preferred) {
            ordered.push(preferred.to_string());
        }
    }

    for offset in 0..account_ids.len() {
        let account_id = &account_ids[(start + offset) % account_ids.len()];
        if ordered.iter().any(|value| value == account_id) {
            continue;
        }
        ordered.push(account_id.clone());
    }

    ordered
}

fn normalize_plan_key(plan_type: Option<&str>) -> String {
    let normalized = plan_type.unwrap_or("").trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return "free".to_string();
    }
    if normalized.contains("enterprise") {
        return "enterprise".to_string();
    }
    if normalized.contains("business") {
        return "business".to_string();
    }
    if normalized.contains("team") {
        return "team".to_string();
    }
    if normalized.contains("edu") {
        return "edu".to_string();
    }
    if normalized.contains("go") {
        return "go".to_string();
    }
    if normalized.contains("plus") {
        return "plus".to_string();
    }
    if normalized.contains("pro") {
        return "pro".to_string();
    }
    if normalized.contains("free") {
        return "free".to_string();
    }
    normalized
}

fn normalize_auth_file_plan_type(plan_type: Option<&str>) -> Option<&'static str> {
    let normalized = plan_type?
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-");
    match normalized.as_str() {
        "prolite" | "pro-lite" => Some("prolite"),
        "promax" | "pro-max" => Some("promax"),
        _ => None,
    }
}

fn resolve_plan_rank(account: &CodexAccount) -> Option<i32> {
    let plan_key = normalize_plan_key(account.plan_type.as_deref());
    let auth_file_plan_type = normalize_auth_file_plan_type(account.auth_file_plan_type.as_deref())
        .or_else(|| normalize_auth_file_plan_type(account.plan_type.as_deref()));

    let rank = match plan_key.as_str() {
        "enterprise" => 700,
        "business" => 650,
        "team" => 640,
        "edu" => 630,
        "pro" => match auth_file_plan_type {
            Some("promax") => 560,
            Some("prolite") => 520,
            _ => 540,
        },
        "plus" => 420,
        "go" => 360,
        "free" => 300,
        _ => return None,
    };

    Some(rank)
}

fn resolve_remaining_quota(account: &CodexAccount) -> Option<i32> {
    let quota = account.quota.as_ref()?;
    let mut percentages = Vec::new();
    if quota.hourly_window_present.unwrap_or(true) {
        percentages.push(quota.hourly_percentage.clamp(0, 100));
    }
    if quota.weekly_window_present.unwrap_or(true) {
        percentages.push(quota.weekly_percentage.clamp(0, 100));
    }
    percentages.into_iter().min()
}

fn build_routing_candidates(ordered_account_ids: &[String]) -> Vec<RoutingCandidate> {
    ordered_account_ids
        .iter()
        .map(|account_id| {
            let account = codex_account::load_account(account_id);
            RoutingCandidate {
                account_id: account_id.clone(),
                plan_rank: account.as_ref().and_then(resolve_plan_rank),
                remaining_quota: account.as_ref().and_then(resolve_remaining_quota),
            }
        })
        .collect()
}

fn compare_routing_candidates(
    left: &RoutingCandidate,
    right: &RoutingCandidate,
    strategy: CodexLocalAccessRoutingStrategy,
    original_index: &HashMap<String, usize>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let compare_option_desc = |a: Option<i32>, b: Option<i32>| match (a, b) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let compare_option_asc = |a: Option<i32>, b: Option<i32>| match (a, b) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };

    let ordering = match strategy {
        CodexLocalAccessRoutingStrategy::Auto => {
            compare_option_desc(left.plan_rank, right.plan_rank)
                .then_with(|| compare_option_desc(left.remaining_quota, right.remaining_quota))
        }
        CodexLocalAccessRoutingStrategy::QuotaHighFirst => {
            compare_option_desc(left.remaining_quota, right.remaining_quota)
                .then_with(|| compare_option_desc(left.plan_rank, right.plan_rank))
        }
        CodexLocalAccessRoutingStrategy::QuotaLowFirst => {
            compare_option_asc(left.remaining_quota, right.remaining_quota)
                .then_with(|| compare_option_desc(left.plan_rank, right.plan_rank))
        }
        CodexLocalAccessRoutingStrategy::PlanHighFirst => {
            compare_option_desc(left.plan_rank, right.plan_rank)
                .then_with(|| compare_option_desc(left.remaining_quota, right.remaining_quota))
        }
        CodexLocalAccessRoutingStrategy::PlanLowFirst => {
            compare_option_asc(left.plan_rank, right.plan_rank)
                .then_with(|| compare_option_desc(left.remaining_quota, right.remaining_quota))
        }
    };

    ordering.then_with(|| {
        let left_index = original_index
            .get(&left.account_id)
            .copied()
            .unwrap_or(usize::MAX);
        let right_index = original_index
            .get(&right.account_id)
            .copied()
            .unwrap_or(usize::MAX);
        left_index.cmp(&right_index)
    })
}

fn apply_routing_strategy(
    account_ids: &[String],
    strategy: CodexLocalAccessRoutingStrategy,
) -> Vec<String> {
    let original_index: HashMap<String, usize> = account_ids
        .iter()
        .enumerate()
        .map(|(index, account_id)| (account_id.clone(), index))
        .collect();
    let mut candidates = build_routing_candidates(account_ids);
    candidates
        .sort_by(|left, right| compare_routing_candidates(left, right, strategy, &original_index));
    candidates
        .into_iter()
        .map(|candidate| candidate.account_id)
        .collect()
}

fn pin_account_to_front(
    account_ids: Vec<String>,
    preferred_account_id: Option<&str>,
) -> Vec<String> {
    let Some(preferred_account_id) = preferred_account_id else {
        return account_ids;
    };
    let preferred_account_id = preferred_account_id.trim();
    if preferred_account_id.is_empty() {
        return account_ids;
    }

    let mut ordered = Vec::with_capacity(account_ids.len());
    if account_ids
        .iter()
        .any(|account_id| account_id == preferred_account_id)
    {
        ordered.push(preferred_account_id.to_string());
    }
    for account_id in account_ids {
        if account_id == preferred_account_id {
            continue;
        }
        ordered.push(account_id);
    }
    ordered
}

fn format_retry_after_duration(wait: Duration) -> String {
    let seconds = wait.as_secs().max(1);
    format!("{} 秒", seconds)
}

fn build_cooldown_unavailable_message(model_key: &str, wait: Duration) -> String {
    let wait_text = format_retry_after_duration(wait);
    if model_key.trim().is_empty() {
        format!("当前 API 服务账号均在冷却中，请 {} 后重试", wait_text)
    } else {
        format!(
            "模型 {} 的可用账号均在冷却中，请 {} 后重试",
            model_key, wait_text,
        )
    }
}

fn parse_codex_retry_after(status: StatusCode, error_body: &str) -> Option<Duration> {
    if status != StatusCode::TOO_MANY_REQUESTS || error_body.trim().is_empty() {
        return None;
    }

    let payload = serde_json::from_str::<Value>(error_body).ok()?;
    let error = payload.get("error")?;
    if error.get("type").and_then(Value::as_str).map(str::trim) != Some("usage_limit_reached") {
        return None;
    }

    let now_seconds = chrono::Utc::now().timestamp();
    if let Some(resets_at) = error.get("resets_at").and_then(Value::as_i64) {
        if resets_at > now_seconds {
            let delta = resets_at.saturating_sub(now_seconds) as u64;
            if delta > 0 {
                return Some(Duration::from_secs(delta));
            }
        }
    }

    error
        .get("resets_in_seconds")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(seconds as u64))
}

fn empty_stats_snapshot() -> CodexLocalAccessStats {
    let now = now_ms();
    let day_since = now.saturating_sub(DAY_WINDOW_MS);
    let week_since = now.saturating_sub(WEEK_WINDOW_MS);
    let month_since = now.saturating_sub(MONTH_WINDOW_MS);
    CodexLocalAccessStats {
        since: now,
        updated_at: now,
        totals: CodexLocalAccessUsageStats::default(),
        accounts: Vec::new(),
        daily: CodexLocalAccessStatsWindow {
            since: day_since,
            updated_at: now,
            totals: CodexLocalAccessUsageStats::default(),
            accounts: Vec::new(),
        },
        weekly: CodexLocalAccessStatsWindow {
            since: week_since,
            updated_at: now,
            totals: CodexLocalAccessUsageStats::default(),
            accounts: Vec::new(),
        },
        monthly: CodexLocalAccessStatsWindow {
            since: month_since,
            updated_at: now,
            totals: CodexLocalAccessUsageStats::default(),
            accounts: Vec::new(),
        },
        events: Vec::new(),
    }
}

fn empty_stats_window(since: i64, updated_at: i64) -> CodexLocalAccessStatsWindow {
    CodexLocalAccessStatsWindow {
        since,
        updated_at,
        totals: CodexLocalAccessUsageStats::default(),
        accounts: Vec::new(),
    }
}

fn sort_usage_accounts(accounts: &mut [CodexLocalAccessAccountStats]) {
    accounts.sort_by(|left, right| {
        right
            .usage
            .request_count
            .cmp(&left.usage.request_count)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
}

fn trim_recent_events(events: &mut Vec<CodexLocalAccessUsageEvent>, month_since: i64) {
    events.retain(|event| event.timestamp > 0 && event.timestamp >= month_since);
    events.sort_by_key(|event| event.timestamp);
    if events.len() > MAX_RECENT_USAGE_EVENTS {
        let remove = events.len().saturating_sub(MAX_RECENT_USAGE_EVENTS);
        events.drain(0..remove);
    }
}

fn append_usage_event(
    events: &mut Vec<CodexLocalAccessUsageEvent>,
    now: i64,
    account_id: Option<&str>,
    account_email: Option<&str>,
    success: bool,
    latency_ms: u64,
    usage: Option<&UsageCapture>,
) {
    let usage = usage.cloned().unwrap_or_default();
    events.push(CodexLocalAccessUsageEvent {
        timestamp: now,
        account_id: account_id.unwrap_or_default().trim().to_string(),
        email: account_email.unwrap_or_default().trim().to_string(),
        success,
        latency_ms,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        cached_tokens: usage.cached_tokens,
        reasoning_tokens: usage.reasoning_tokens,
    });
}

fn apply_usage_event_to_window(
    window: &mut CodexLocalAccessStatsWindow,
    event: &CodexLocalAccessUsageEvent,
) {
    let usage = UsageCapture {
        input_tokens: event.input_tokens,
        output_tokens: event.output_tokens,
        total_tokens: event.total_tokens,
        cached_tokens: event.cached_tokens,
        reasoning_tokens: event.reasoning_tokens,
    };
    apply_usage_stats(
        &mut window.totals,
        event.success,
        event.latency_ms,
        Some(&usage),
    );
    upsert_account_usage_stats(
        &mut window.accounts,
        Some(event.account_id.as_str()),
        Some(event.email.as_str()),
        event.success,
        event.latency_ms,
        Some(&usage),
        event.timestamp,
    );
    window.updated_at = window.updated_at.max(event.timestamp);
}

fn recompute_time_windows(stats: &mut CodexLocalAccessStats, now: i64) {
    let day_since = now.saturating_sub(DAY_WINDOW_MS);
    let week_since = now.saturating_sub(WEEK_WINDOW_MS);
    let month_since = now.saturating_sub(MONTH_WINDOW_MS);

    trim_recent_events(&mut stats.events, month_since);

    let mut daily = empty_stats_window(day_since, stats.updated_at.max(day_since));
    let mut weekly = empty_stats_window(week_since, stats.updated_at.max(week_since));
    let mut monthly = empty_stats_window(month_since, stats.updated_at.max(month_since));

    for event in &stats.events {
        if event.timestamp >= month_since {
            apply_usage_event_to_window(&mut monthly, event);
        }
        if event.timestamp >= week_since {
            apply_usage_event_to_window(&mut weekly, event);
        }
        if event.timestamp >= day_since {
            apply_usage_event_to_window(&mut daily, event);
        }
    }

    sort_usage_accounts(&mut daily.accounts);
    sort_usage_accounts(&mut weekly.accounts);
    sort_usage_accounts(&mut monthly.accounts);

    stats.daily = daily;
    stats.weekly = weekly;
    stats.monthly = monthly;
}

fn build_base_url(port: u16) -> String {
    format!("http://127.0.0.1:{}/v1", port)
}

fn build_runtime_account(base_url: String, api_key: String) -> CodexAccount {
    let mut runtime_account = CodexAccount::new_api_key(
        "codex_local_access_runtime".to_string(),
        "api-service-local".to_string(),
        api_key,
        CodexApiProviderMode::Custom,
        Some(base_url),
        Some("codex_local_access".to_string()),
        Some("Codex API Service".to_string()),
    );
    runtime_account.account_name = Some("API Service".to_string());
    runtime_account
}

fn generate_local_api_key() -> String {
    let suffix: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    format!("agt_codex_{}", suffix)
}

fn allocate_random_local_port() -> Result<u16, String> {
    let listener = StdTcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("分配本地接入端口失败: {}", e))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| format!("读取本地接入端口失败: {}", e))
}

fn load_collection_from_disk() -> Result<Option<CodexLocalAccessCollection>, String> {
    let path = local_access_file_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("读取本地接入配置失败: {}", e))?;
    let parsed = serde_json::from_str::<CodexLocalAccessCollection>(&content)
        .map_err(|e| format!("解析本地接入配置失败: {}", e))?;
    Ok(Some(parsed))
}

fn save_collection_to_disk(collection: &CodexLocalAccessCollection) -> Result<(), String> {
    let path = local_access_file_path()?;
    let content = serde_json::to_string_pretty(collection)
        .map_err(|e| format!("序列化本地接入配置失败: {}", e))?;
    write_string_atomic(&path, &content)
}

fn normalize_stats(stats: &mut CodexLocalAccessStats) {
    let now = now_ms();
    if stats.since <= 0 {
        stats.since = now;
    }
    if stats.updated_at <= 0 {
        stats.updated_at = stats.since;
    }
    sort_usage_accounts(&mut stats.accounts);
    recompute_time_windows(stats, now);
}

fn load_stats_from_disk() -> Result<CodexLocalAccessStats, String> {
    let path = local_access_stats_file_path()?;
    if !path.exists() {
        return Ok(empty_stats_snapshot());
    }

    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("读取 API 服务统计失败: {}", e))?;
    let mut parsed = serde_json::from_str::<CodexLocalAccessStats>(&content)
        .map_err(|e| format!("解析 API 服务统计失败: {}", e))?;
    normalize_stats(&mut parsed);
    Ok(parsed)
}

fn save_stats_to_disk(stats: &CodexLocalAccessStats) -> Result<(), String> {
    let path = local_access_stats_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 API 服务统计目录失败: {}", e))?;
    }
    let content = serde_json::to_string_pretty(stats)
        .map_err(|e| format!("序列化 API 服务统计失败: {}", e))?;
    write_string_atomic(&path, &content)
}

fn prune_runtime_routing_state(runtime: &mut GatewayRuntime, now: i64) {
    runtime
        .response_affinity
        .retain(|_, binding| now.saturating_sub(binding.updated_at_ms) <= RESPONSE_AFFINITY_TTL_MS);
    runtime
        .model_cooldowns
        .retain(|_, cooldown| cooldown.next_retry_at_ms > now);

    if runtime.response_affinity.len() <= MAX_RESPONSE_AFFINITY_BINDINGS {
        return;
    }

    let mut bindings: Vec<(String, i64)> = runtime
        .response_affinity
        .iter()
        .map(|(response_id, binding)| (response_id.clone(), binding.updated_at_ms))
        .collect();
    bindings.sort_by_key(|(_, updated_at_ms)| *updated_at_ms);

    let remove_count = runtime
        .response_affinity
        .len()
        .saturating_sub(MAX_RESPONSE_AFFINITY_BINDINGS);
    for (response_id, _) in bindings.into_iter().take(remove_count) {
        runtime.response_affinity.remove(&response_id);
    }
}

async fn resolve_affinity_account(previous_response_id: &str) -> Option<String> {
    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    runtime
        .response_affinity
        .get(previous_response_id)
        .map(|binding| binding.account_id.clone())
}

async fn bind_response_affinity(response_id: &str, account_id: &str) {
    let response_id = response_id.trim();
    let account_id = account_id.trim();
    if response_id.is_empty() || account_id.is_empty() {
        return;
    }

    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    runtime.response_affinity.insert(
        response_id.to_string(),
        ResponseAffinityBinding {
            account_id: account_id.to_string(),
            updated_at_ms: now,
        },
    );
    prune_runtime_routing_state(&mut runtime, now);
}

async fn clear_model_cooldown(account_id: &str, model_key: &str) {
    let Some(cooldown_key) = build_cooldown_key(account_id, model_key) else {
        return;
    };

    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    runtime.model_cooldowns.remove(&cooldown_key);
}

async fn set_model_cooldown(account_id: &str, model_key: &str, retry_after: Duration) {
    let Some(cooldown_key) = build_cooldown_key(account_id, model_key) else {
        return;
    };
    if retry_after <= Duration::ZERO {
        return;
    }

    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    let next_retry_at_ms = now.saturating_add(retry_after.as_millis() as i64);
    prune_runtime_routing_state(&mut runtime, now);
    runtime
        .model_cooldowns
        .insert(cooldown_key, AccountModelCooldown { next_retry_at_ms });
}

async fn get_model_cooldown_wait(account_id: &str, model_key: &str) -> Option<Duration> {
    let cooldown_key = build_cooldown_key(account_id, model_key)?;
    let mut runtime = gateway_runtime().lock().await;
    let now = now_ms();
    prune_runtime_routing_state(&mut runtime, now);
    let cooldown = runtime.model_cooldowns.get(&cooldown_key)?;
    let wait_ms = cooldown.next_retry_at_ms.saturating_sub(now);
    if wait_ms <= 0 {
        return None;
    }
    Some(Duration::from_millis(wait_ms as u64))
}

fn ensure_local_port_available(port: u16, current_port: Option<u16>) -> Result<(), String> {
    if port == 0 {
        return Err("端口必须在 1 到 65535 之间".to_string());
    }
    if current_port == Some(port) {
        return Ok(());
    }
    let listener = StdTcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("端口 {} 不可用: {}", port, e))?;
    drop(listener);
    Ok(())
}

fn is_free_plan_type(plan_type: Option<&str>) -> bool {
    let Some(plan_type) = plan_type else {
        return false;
    };
    let normalized = plan_type.trim().to_ascii_lowercase();
    !normalized.is_empty() && normalized.contains("free")
}

fn is_local_access_eligible_account(account: &CodexAccount) -> bool {
    !account.is_api_key_auth() && !is_free_plan_type(account.plan_type.as_deref())
}

fn sanitize_collection(
    collection: &mut CodexLocalAccessCollection,
) -> Result<(bool, HashSet<String>), String> {
    let mut changed = false;

    if collection.port == 0 {
        collection.port = allocate_random_local_port()?;
        changed = true;
    }
    if collection.api_key.trim().is_empty() {
        collection.api_key = generate_local_api_key();
        changed = true;
    }
    if collection.created_at <= 0 {
        collection.created_at = now_ms();
        changed = true;
    }
    if collection.updated_at <= 0 {
        collection.updated_at = now_ms();
        changed = true;
    }

    let valid_account_ids: HashSet<String> = codex_account::list_accounts_checked()?
        .into_iter()
        .filter(is_local_access_eligible_account)
        .map(|account| account.id)
        .collect();

    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for account_id in &collection.account_ids {
        if !valid_account_ids.contains(account_id) {
            changed = true;
            continue;
        }
        if !seen.insert(account_id.clone()) {
            changed = true;
            continue;
        }
        deduped.push(account_id.clone());
    }
    if deduped != collection.account_ids {
        collection.account_ids = deduped;
        changed = true;
    }

    Ok((changed, valid_account_ids))
}

async fn ensure_runtime_loaded() -> Result<(), String> {
    {
        let runtime = gateway_runtime().lock().await;
        if runtime.loaded {
            return Ok(());
        }
    }

    let loaded_collection = load_collection_from_disk()?;
    let mut loaded_stats = load_stats_from_disk()?;
    let mut next_collection = loaded_collection;
    let mut persist_after_load = false;

    if next_collection.is_none() {
        next_collection = Some(CodexLocalAccessCollection {
            enabled: false,
            port: allocate_random_local_port()?,
            api_key: generate_local_api_key(),
            routing_strategy: CodexLocalAccessRoutingStrategy::default(),
            account_ids: Vec::new(),
            created_at: now_ms(),
            updated_at: now_ms(),
        });
        persist_after_load = true;
    }

    if let Some(collection) = next_collection.as_mut() {
        let (changed, _) = sanitize_collection(collection)?;
        persist_after_load = persist_after_load || changed;
    }

    if persist_after_load {
        if let Some(collection) = next_collection.as_ref() {
            save_collection_to_disk(collection)?;
        }
    }

    let should_start = next_collection
        .as_ref()
        .map(|collection| collection.enabled)
        .unwrap_or(false);

    {
        let mut runtime = gateway_runtime().lock().await;
        runtime.loaded = true;
        runtime.collection = next_collection.clone();
        normalize_stats(&mut loaded_stats);
        runtime.stats = loaded_stats;
        if next_collection.is_none() {
            runtime.last_error = None;
        }
    }

    if should_start {
        ensure_gateway_matches_runtime().await?;
    }

    Ok(())
}

async fn ensure_gateway_matches_runtime() -> Result<(), String> {
    let (collection, running, actual_port, stale_task) = {
        let mut runtime = gateway_runtime().lock().await;
        let stale_task = if !runtime.running {
            runtime.task.take()
        } else {
            None
        };
        (
            runtime.collection.clone(),
            runtime.running,
            runtime.actual_port,
            stale_task,
        )
    };

    if let Some(task) = stale_task {
        let _ = task.await;
    }

    let Some(collection) = collection else {
        stop_gateway().await;
        return Ok(());
    };

    if !collection.enabled {
        stop_gateway().await;
        return Ok(());
    }

    if running && actual_port == Some(collection.port) {
        return Ok(());
    }

    stop_gateway().await;

    let listener = TcpListener::bind(("127.0.0.1", collection.port))
        .await
        .map_err(|e| format!("启动本地接入服务失败: {}", e))?;
    let (shutdown_sender, mut shutdown_receiver) = watch::channel(false);
    let port = collection.port;

    let task = tokio::spawn(async move {
        logger::log_info(&format!(
            "[CodexLocalAccess] 本地接入服务已启动: {}",
            build_base_url(port)
        ));

        loop {
            tokio::select! {
                changed = shutdown_receiver.changed() => {
                    if changed.is_ok() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, addr)) => {
                            tokio::spawn(async move {
                                if let Err(err) = handle_connection(stream).await {
                                    logger::log_warn(&format!(
                                        "[CodexLocalAccess] 请求处理失败 {}: {}",
                                        addr, err
                                    ));
                                }
                            });
                        }
                        Err(err) => {
                            logger::log_warn(&format!(
                                "[CodexLocalAccess] 接收请求失败: {}",
                                err
                            ));
                            break;
                        }
                    }
                }
            }
        }

        let mut runtime = gateway_runtime().lock().await;
        if runtime.actual_port == Some(port) {
            runtime.running = false;
            runtime.actual_port = None;
            runtime.shutdown_sender = None;
        }
    });

    let mut runtime = gateway_runtime().lock().await;
    runtime.running = true;
    runtime.actual_port = Some(collection.port);
    runtime.last_error = None;
    runtime.shutdown_sender = Some(shutdown_sender);
    runtime.task = Some(task);
    Ok(())
}

async fn stop_gateway() {
    let (shutdown_sender, task) = {
        let mut runtime = gateway_runtime().lock().await;
        runtime.running = false;
        runtime.actual_port = None;
        (runtime.shutdown_sender.take(), runtime.task.take())
    };

    if let Some(sender) = shutdown_sender {
        let _ = sender.send(true);
    }
    if let Some(task) = task {
        let _ = task.await;
    }
}

fn apply_usage_stats(
    target: &mut CodexLocalAccessUsageStats,
    success: bool,
    latency_ms: u64,
    usage: Option<&UsageCapture>,
) {
    target.request_count = target.request_count.saturating_add(1);
    if success {
        target.success_count = target.success_count.saturating_add(1);
    } else {
        target.failure_count = target.failure_count.saturating_add(1);
    }
    target.total_latency_ms = target.total_latency_ms.saturating_add(latency_ms);

    if let Some(usage) = usage {
        target.input_tokens = target.input_tokens.saturating_add(usage.input_tokens);
        target.output_tokens = target.output_tokens.saturating_add(usage.output_tokens);
        target.total_tokens = target.total_tokens.saturating_add(usage.total_tokens);
        target.cached_tokens = target.cached_tokens.saturating_add(usage.cached_tokens);
        target.reasoning_tokens = target
            .reasoning_tokens
            .saturating_add(usage.reasoning_tokens);
    }
}

fn upsert_account_usage_stats(
    accounts: &mut Vec<CodexLocalAccessAccountStats>,
    account_id: Option<&str>,
    account_email: Option<&str>,
    success: bool,
    latency_ms: u64,
    usage: Option<&UsageCapture>,
    updated_at: i64,
) {
    let Some(account_id) = account_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    let normalized_email = account_email
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string();

    if let Some(account_stats) = accounts
        .iter_mut()
        .find(|item| item.account_id == account_id)
    {
        if !normalized_email.is_empty() {
            account_stats.email = normalized_email;
        }
        account_stats.updated_at = updated_at;
        apply_usage_stats(&mut account_stats.usage, success, latency_ms, usage);
        return;
    }

    let mut account_stats = CodexLocalAccessAccountStats {
        account_id: account_id.to_string(),
        email: normalized_email,
        usage: CodexLocalAccessUsageStats::default(),
        updated_at,
    };
    apply_usage_stats(&mut account_stats.usage, success, latency_ms, usage);
    accounts.push(account_stats);
}

async fn record_request_stats(
    account_id: Option<&str>,
    account_email: Option<&str>,
    success: bool,
    latency_ms: u64,
    usage: Option<UsageCapture>,
) -> Result<(), String> {
    let stats_snapshot = {
        let mut runtime = gateway_runtime().lock().await;
        let now = now_ms();
        let usage_ref = usage.as_ref();
        if runtime.stats.since <= 0 {
            runtime.stats.since = now;
        }
        runtime.stats.updated_at = now;
        apply_usage_stats(&mut runtime.stats.totals, success, latency_ms, usage_ref);
        upsert_account_usage_stats(
            &mut runtime.stats.accounts,
            account_id,
            account_email,
            success,
            latency_ms,
            usage_ref,
            now,
        );
        append_usage_event(
            &mut runtime.stats.events,
            now,
            account_id,
            account_email,
            success,
            latency_ms,
            usage_ref,
        );

        normalize_stats(&mut runtime.stats);
        runtime.stats.clone()
    };

    save_stats_to_disk(&stats_snapshot)
}

fn build_state_snapshot(runtime: &GatewayRuntime) -> CodexLocalAccessState {
    let collection = runtime.collection.clone();
    let member_count = collection
        .as_ref()
        .map(|item| item.account_ids.len())
        .unwrap_or(0);
    let base_url = collection.as_ref().map(|item| build_base_url(item.port));
    let mut stats = runtime.stats.clone();
    stats.events.clear();

    CodexLocalAccessState {
        collection,
        running: runtime.running,
        base_url,
        last_error: runtime.last_error.clone(),
        member_count,
        stats,
    }
}

async fn snapshot_state() -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;
    ensure_gateway_matches_runtime().await?;
    let runtime = gateway_runtime().lock().await;
    Ok(build_state_snapshot(&runtime))
}

pub async fn get_local_access_state() -> Result<CodexLocalAccessState, String> {
    snapshot_state().await
}

pub async fn activate_local_access_for_dir(
    profile_dir: &Path,
) -> Result<CodexLocalAccessState, String> {
    let state = set_local_access_enabled(true).await?;
    let collection = state
        .collection
        .clone()
        .ok_or_else(|| "API 服务集合尚未创建".to_string())?;
    let base_url = state
        .base_url
        .clone()
        .unwrap_or_else(|| build_base_url(collection.port));
    let runtime_account = build_runtime_account(base_url, collection.api_key.clone());
    codex_account::write_account_bundle_to_dir(profile_dir, &runtime_account)?;
    Ok(state)
}

pub async fn save_local_access_accounts(
    account_ids: Vec<String>,
) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let mut collection = {
        let runtime = gateway_runtime().lock().await;
        runtime
            .collection
            .clone()
            .unwrap_or(CodexLocalAccessCollection {
                enabled: false,
                port: allocate_random_local_port()?,
                api_key: generate_local_api_key(),
                routing_strategy: CodexLocalAccessRoutingStrategy::default(),
                account_ids: Vec::new(),
                created_at: now_ms(),
                updated_at: now_ms(),
            })
    };

    let valid_account_ids: HashSet<String> = codex_account::list_accounts_checked()?
        .into_iter()
        .filter(is_local_access_eligible_account)
        .map(|account| account.id)
        .collect();

    let mut next_account_ids = Vec::new();
    let mut seen = HashSet::new();
    for account_id in account_ids {
        if !valid_account_ids.contains(&account_id) {
            continue;
        }
        if seen.insert(account_id.clone()) {
            next_account_ids.push(account_id);
        }
    }

    collection.account_ids = next_account_ids;
    collection.updated_at = now_ms();
    let (changed, _) = sanitize_collection(&mut collection)?;
    if changed {
        collection.updated_at = now_ms();
    }
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        runtime.collection = Some(collection);
        runtime.loaded = true;
        runtime.last_error = None;
    }

    ensure_gateway_matches_runtime().await?;
    snapshot_state().await
}

pub async fn update_local_access_routing_strategy(
    strategy: CodexLocalAccessRoutingStrategy,
) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return Err("本地接入集合尚未创建".to_string());
    };

    if collection.routing_strategy == strategy {
        return snapshot_state().await;
    }

    collection.routing_strategy = strategy;
    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        runtime.collection = Some(collection);
        runtime.loaded = true;
        runtime.last_error = None;
    }

    snapshot_state().await
}

pub async fn remove_local_access_account(
    account_id: &str,
) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return snapshot_state().await;
    };

    let before_len = collection.account_ids.len();
    collection.account_ids.retain(|id| id != account_id);
    if collection.account_ids.len() == before_len {
        return snapshot_state().await;
    }

    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        runtime.collection = Some(collection);
        runtime.loaded = true;
        runtime.last_error = None;
    }

    ensure_gateway_matches_runtime().await?;
    snapshot_state().await
}

pub async fn rotate_local_access_api_key() -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return Err("本地接入集合尚未创建".to_string());
    };

    collection.api_key = generate_local_api_key();
    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        runtime.collection = Some(collection);
        runtime.loaded = true;
        runtime.last_error = None;
    }

    snapshot_state().await
}

pub async fn clear_local_access_stats() -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let cleared = empty_stats_snapshot();
    save_stats_to_disk(&cleared)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        runtime.stats = cleared;
    }

    snapshot_state().await
}

pub async fn update_local_access_port(port: u16) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return Err("本地接入集合尚未创建".to_string());
    };

    ensure_local_port_available(port, Some(collection.port))?;
    if collection.port == port {
        return snapshot_state().await;
    }

    collection.port = port;
    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        runtime.collection = Some(collection);
        runtime.loaded = true;
        runtime.last_error = None;
    }

    ensure_gateway_matches_runtime().await?;
    snapshot_state().await
}

pub async fn set_local_access_enabled(enabled: bool) -> Result<CodexLocalAccessState, String> {
    ensure_runtime_loaded().await?;

    let maybe_collection = {
        let runtime = gateway_runtime().lock().await;
        runtime.collection.clone()
    };

    let Some(mut collection) = maybe_collection else {
        return Err("本地接入集合尚未创建".to_string());
    };

    collection.enabled = enabled;
    collection.updated_at = now_ms();
    save_collection_to_disk(&collection)?;

    {
        let mut runtime = gateway_runtime().lock().await;
        runtime.collection = Some(collection);
        runtime.loaded = true;
        runtime.last_error = None;
    }

    ensure_gateway_matches_runtime().await?;
    snapshot_state().await
}

pub async fn restore_local_access_gateway() {
    if let Err(err) = ensure_runtime_loaded().await {
        let mut runtime = gateway_runtime().lock().await;
        runtime.loaded = true;
        runtime.last_error = Some(err.clone());
        logger::log_warn(&format!("[CodexLocalAccess] 初始化失败: {}", err));
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn parse_content_length(header_bytes: &[u8]) -> Result<usize, String> {
    let header_text = String::from_utf8_lossy(header_bytes);
    for line in header_text.lines() {
        let mut parts = line.splitn(2, ':');
        let Some(name) = parts.next() else { continue };
        let Some(value) = parts.next() else { continue };
        if name.trim().eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse::<usize>()
                .map_err(|e| format!("非法 Content-Length: {}", e));
        }
    }
    Ok(0)
}

async fn read_http_request<R>(stream: &mut R) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 2048];
    let mut header_end: Option<usize> = None;
    let mut content_length = 0usize;

    loop {
        let bytes_read = timeout(REQUEST_READ_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| "读取请求超时".to_string())?
            .map_err(|e| format!("读取请求失败: {}", e))?;

        if bytes_read == 0 {
            break;
        }

        buffer.extend_from_slice(&chunk[..bytes_read]);
        if buffer.len() > MAX_HTTP_REQUEST_BYTES {
            return Err("请求体过大".to_string());
        }

        if header_end.is_none() {
            if let Some(end) = find_header_end(&buffer) {
                content_length = parse_content_length(&buffer[..end])?;
                header_end = Some(end);
            }
        }

        if let Some(end) = header_end {
            if buffer.len() >= end.saturating_add(content_length) {
                return Ok(buffer[..(end + content_length)].to_vec());
            }
        }
    }

    Err("请求不完整".to_string())
}

fn parse_http_request(raw: &[u8]) -> Result<ParsedRequest, String> {
    let Some(header_end) = find_header_end(raw) else {
        return Err("缺少 HTTP 头结束标记".to_string());
    };

    let header_text = String::from_utf8_lossy(&raw[..header_end]);
    let mut lines = header_text.lines();
    let request_line = lines.next().ok_or("请求行为空")?.trim();

    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("请求行缺少 method")?.to_string();
    let target = parts.next().ok_or("请求行缺少 target")?.to_string();

    let mut headers = HashMap::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, ':');
        let Some(name) = parts.next() else { continue };
        let Some(value) = parts.next() else { continue };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    Ok(ParsedRequest {
        method,
        target,
        headers,
        body: raw[header_end..].to_vec(),
    })
}

fn normalize_proxy_target(target: &str) -> Result<String, String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        let parsed = url::Url::parse(target).map_err(|e| format!("解析请求地址失败: {}", e))?;
        let mut next = parsed.path().to_string();
        if let Some(query) = parsed.query() {
            next.push('?');
            next.push_str(query);
        }
        return Ok(next);
    }

    let parsed = url::Url::parse(&format!("http://localhost{}", target))
        .map_err(|e| format!("解析请求路径失败: {}", e))?;
    let mut next = parsed.path().to_string();
    if let Some(query) = parsed.query() {
        next.push('?');
        next.push_str(query);
    }
    Ok(next)
}

fn extract_local_api_key(headers: &HashMap<String, String>) -> Option<String> {
    if let Some(value) = headers.get("authorization") {
        let trimmed = value.trim();
        if let Some(rest) = trimmed.strip_prefix("Bearer ") {
            let token = rest.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
        if let Some(rest) = trimmed.strip_prefix("bearer ") {
            let token = rest.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }

    headers
        .get("x-api-key")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_local_models_request(target: &str) -> bool {
    target == "/v1/models" || target.starts_with("/v1/models?")
}

fn build_local_models_response() -> Value {
    let data: Vec<Value> = DEFAULT_CODEX_MODELS
        .iter()
        .map(|model| {
            json!({
                "id": model,
                "object": "model",
                "created": 0,
                "owned_by": "openai",
            })
        })
        .collect();

    json!({
        "object": "list",
        "data": data,
    })
}

fn usage_number(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).or_else(|| {
        value
            .and_then(Value::as_i64)
            .filter(|number| *number >= 0)
            .map(|number| number as u64)
    })
}

fn non_null_child<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|item| !item.is_null())
}

fn extract_usage_capture(value: &Value) -> Option<UsageCapture> {
    let usage = non_null_child(value, "usage")
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| non_null_child(item, "usage"))
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| item.get("response"))
                .and_then(|item| non_null_child(item, "usage"))
        })
        .or_else(|| non_null_child(value, "usageMetadata"))
        .or_else(|| non_null_child(value, "usage_metadata"))
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| non_null_child(item, "usageMetadata"))
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| non_null_child(item, "usage_metadata"))
        })?;

    let input_tokens = usage_number(
        usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens"))
            .or_else(|| usage.get("promptTokenCount")),
    )
    .unwrap_or(0);
    let output_tokens = usage_number(
        usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens"))
            .or_else(|| usage.get("candidatesTokenCount")),
    )
    .unwrap_or(0);
    let explicit_total_tokens = usage_number(
        usage
            .get("total_tokens")
            .or_else(|| usage.get("totalTokenCount")),
    );
    let cached_tokens = usage_number(
        usage
            .get("cached_tokens")
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|item| item.get("cached_tokens"))
            })
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|item| item.get("cached_tokens"))
            })
            .or_else(|| usage.get("cachedContentTokenCount")),
    )
    .unwrap_or(0);
    let reasoning_tokens = usage_number(
        usage
            .get("reasoning_tokens")
            .or_else(|| {
                usage
                    .get("output_tokens_details")
                    .and_then(|item| item.get("reasoning_tokens"))
            })
            .or_else(|| {
                usage
                    .get("completion_tokens_details")
                    .and_then(|item| item.get("reasoning_tokens"))
            })
            .or_else(|| usage.get("thoughtsTokenCount")),
    )
    .unwrap_or(0);

    Some(UsageCapture {
        input_tokens,
        output_tokens,
        total_tokens: if explicit_total_tokens.unwrap_or(0) == 0 {
            input_tokens
                .saturating_add(output_tokens)
                .saturating_add(reasoning_tokens)
        } else {
            explicit_total_tokens.unwrap_or(0)
        },
        cached_tokens,
        reasoning_tokens,
    })
}

fn extract_response_id(value: &Value) -> Option<String> {
    non_null_child(value, "id")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("response")
                .and_then(|item| non_null_child(item, "id"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn should_treat_response_as_stream(content_type: &str, request_is_stream: bool) -> bool {
    request_is_stream
        || content_type
            .to_ascii_lowercase()
            .contains("text/event-stream")
}

fn find_sse_frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    if buffer.len() < 2 {
        return None;
    }

    for index in 0..buffer.len().saturating_sub(1) {
        if index + 3 < buffer.len() && &buffer[index..index + 4] == b"\r\n\r\n" {
            return Some((index, 4));
        }
        if &buffer[index..index + 2] == b"\n\n" {
            return Some((index, 2));
        }
    }

    None
}

impl ResponseUsageCollector {
    fn new(is_stream: bool) -> Self {
        Self {
            is_stream,
            body: Vec::new(),
            stream_buffer: Vec::new(),
            usage: None,
            response_id: None,
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }

        if self.is_stream {
            self.feed_stream_chunk(chunk);
        } else {
            self.body.extend_from_slice(chunk);
        }
    }

    fn finish(mut self) -> ResponseCapture {
        if self.is_stream {
            self.process_stream_buffer(true);
            ResponseCapture {
                usage: self.usage,
                response_id: self.response_id,
            }
        } else {
            let parsed = serde_json::from_slice::<Value>(&self.body).ok();
            ResponseCapture {
                usage: parsed.as_ref().and_then(extract_usage_capture),
                response_id: parsed.as_ref().and_then(extract_response_id),
            }
        }
    }

    fn feed_stream_chunk(&mut self, chunk: &[u8]) {
        self.stream_buffer.extend_from_slice(chunk);
        self.process_stream_buffer(false);
    }

    fn process_stream_buffer(&mut self, flush_tail: bool) {
        loop {
            let Some((boundary_index, separator_len)) =
                find_sse_frame_boundary(&self.stream_buffer)
            else {
                break;
            };
            let frame = self.stream_buffer[..boundary_index].to_vec();
            self.stream_buffer.drain(..boundary_index + separator_len);
            self.process_stream_frame(&frame);
        }

        if flush_tail && !self.stream_buffer.is_empty() {
            let frame = std::mem::take(&mut self.stream_buffer);
            self.process_stream_frame(&frame);
        }
    }

    fn process_stream_frame(&mut self, frame: &[u8]) {
        if frame.is_empty() {
            return;
        }

        let text = String::from_utf8_lossy(frame);
        let mut data_lines = Vec::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if let Some(rest) = line.strip_prefix("data:") {
                let payload = rest.trim();
                if !payload.is_empty() {
                    data_lines.push(payload.to_string());
                }
            }
        }

        let payload = if data_lines.is_empty() {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }
            trimmed.to_string()
        } else {
            data_lines.join("\n")
        };

        if payload == "[DONE]" {
            return;
        }

        if let Ok(value) = serde_json::from_str::<Value>(&payload) {
            if let Some(usage) = extract_usage_capture(&value) {
                self.usage = Some(usage);
            }
            if self.response_id.is_none() {
                self.response_id = extract_response_id(&value);
            }
        }
    }
}

fn resolve_upstream_target(target: &str) -> Result<String, String> {
    if !target.starts_with("/v1") {
        return Err("仅支持 /v1 路径".to_string());
    }

    let trimmed = target.trim_start_matches("/v1");
    if trimmed.is_empty() {
        Ok("/".to_string())
    } else if trimmed.starts_with('/') {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("/{}", trimmed))
    }
}

fn is_stream_request(headers: &HashMap<String, String>, body: &[u8]) -> bool {
    if let Some(accept) = headers.get("accept") {
        if accept.to_ascii_lowercase().contains("text/event-stream") {
            return true;
        }
    }

    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn resolve_upstream_account_id(account: &CodexAccount) -> Option<String> {
    account
        .account_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            codex_account::extract_chatgpt_account_id_from_access_token(
                &account.tokens.access_token,
            )
        })
}

fn extract_upstream_error_message(body: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(body).ok()?;

    if let Some(message) = parsed
        .get("error")
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
    {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Some(message) = parsed
        .get("detail")
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
    {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Some(message) = parsed.get("message").and_then(Value::as_str) {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Some(message) = parsed.get("error").and_then(Value::as_str) {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

fn summarize_upstream_error(status: StatusCode, body: &str) -> String {
    let detail = extract_upstream_error_message(body).unwrap_or_else(|| {
        let trimmed = body.trim();
        if trimmed.is_empty() {
            format!("上游接口返回状态 {}", status.as_u16())
        } else {
            trimmed.to_string()
        }
    });

    format!("{}: {}", status.as_u16(), detail)
}

fn should_try_next_account(status: StatusCode, body: &str) -> bool {
    if status == StatusCode::UNAUTHORIZED {
        return true;
    }

    let lower = body.to_ascii_lowercase();
    let quota_exhausted = lower.contains("usage_limit_reached")
        || lower.contains("limit reached")
        || lower.contains("insufficient_quota")
        || lower.contains("quota exceeded")
        || lower.contains("quota exceeded");
    let model_capacity =
        lower.contains("selected model is at capacity") || lower.contains("model is at capacity");

    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::FORBIDDEN
    ) && (quota_exhausted || model_capacity)
}

fn json_response(status: u16, status_text: &str, body: &Value) -> Vec<u8> {
    let body_bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n\r\n",
        status,
        status_text,
        body_bytes.len(),
        CORS_ALLOW_HEADERS
    );
    let mut response = headers.into_bytes();
    response.extend_from_slice(&body_bytes);
    response
}

fn options_response() -> Vec<u8> {
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 0\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n\r\n",
        CORS_ALLOW_HEADERS
    );
    headers.into_bytes()
}

async fn write_upstream_response(
    stream: &mut TcpStream,
    upstream: reqwest::Response,
    request_is_stream: bool,
) -> Result<ResponseCapture, String> {
    let status = upstream.status();
    let status_text = status.canonical_reason().unwrap_or("OK");
    let headers = upstream.headers().clone();
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json; charset=utf-8");
    let is_stream = should_treat_response_as_stream(content_type, request_is_stream);

    let mut response_headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n",
        status.as_u16(),
        status_text,
        content_type,
        CORS_ALLOW_HEADERS
    );

    for header_name in ["x-request-id", "openai-processing-ms"] {
        if let Some(value) = headers.get(header_name).and_then(|item| item.to_str().ok()) {
            response_headers.push_str(&format!("{}: {}\r\n", header_name, value));
        }
    }

    response_headers.push_str("\r\n");
    stream
        .write_all(response_headers.as_bytes())
        .await
        .map_err(|e| format!("写入响应头失败: {}", e))?;

    let mut usage_collector = ResponseUsageCollector::new(is_stream);
    let mut body_stream = upstream.bytes_stream();
    while let Some(chunk_result) = body_stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("读取上游响应失败: {}", e))?;
        if chunk.is_empty() {
            continue;
        }
        let prefix = format!("{:X}\r\n", chunk.len());
        stream
            .write_all(prefix.as_bytes())
            .await
            .map_err(|e| format!("写入响应分块前缀失败: {}", e))?;
        stream
            .write_all(&chunk)
            .await
            .map_err(|e| format!("写入响应分块失败: {}", e))?;
        usage_collector.feed(&chunk);
        stream
            .write_all(b"\r\n")
            .await
            .map_err(|e| format!("写入响应分块结束失败: {}", e))?;
    }

    stream
        .write_all(b"0\r\n\r\n")
        .await
        .map_err(|e| format!("写入响应结束失败: {}", e))?;
    Ok(usage_collector.finish())
}

async fn force_refresh_gateway_account(account_id: &str) -> Result<(), String> {
    let mut account = codex_account::load_account(account_id)
        .ok_or_else(|| format!("账号不存在: {}", account_id))?;
    let refresh_token = account
        .tokens
        .refresh_token
        .clone()
        .filter(|token| !token.trim().is_empty())
        .ok_or("当前账号缺少 refresh_token，无法刷新".to_string())?;

    account.tokens = codex_oauth::refresh_access_token_with_fallback(
        &refresh_token,
        Some(account.tokens.id_token.as_str()),
    )
    .await?;
    codex_account::save_account(&account)?;
    Ok(())
}

async fn send_upstream_request(
    method: &str,
    target: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    account: &CodexAccount,
) -> Result<reqwest::Response, String> {
    let method =
        Method::from_bytes(method.as_bytes()).map_err(|e| format!("不支持的请求方法: {}", e))?;
    let url = format!("{}{}", UPSTREAM_CODEX_BASE_URL, target);
    let client = reqwest::Client::new();
    let mut request = client.request(method, &url);

    for (name, value) in headers {
        if matches!(
            name.as_str(),
            "authorization"
                | "host"
                | "content-length"
                | "connection"
                | "accept-encoding"
                | "x-api-key"
        ) {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| format!("无效请求头 {}: {}", name, e))?;
        let header_value =
            HeaderValue::from_str(value).map_err(|e| format!("无效请求头值 {}: {}", name, e))?;
        request = request.header(header_name, header_value);
    }

    request = request.header(
        AUTHORIZATION,
        format!("Bearer {}", account.tokens.access_token.trim()),
    );
    if !headers.contains_key("user-agent") {
        request = request.header(USER_AGENT, DEFAULT_CODEX_USER_AGENT);
    }
    if !headers.contains_key("originator") {
        request = request.header("Originator", DEFAULT_CODEX_ORIGINATOR);
    }
    if let Some(account_id) = resolve_upstream_account_id(account) {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    if !headers.contains_key("accept") {
        request = request.header(
            ACCEPT,
            if is_stream_request(headers, body) {
                "text/event-stream"
            } else {
                "application/json"
            },
        );
    }
    request = request.header("Connection", "Keep-Alive");
    if !headers.contains_key("content-type") && !body.is_empty() {
        request = request.header(CONTENT_TYPE, "application/json");
    }
    if !body.is_empty() {
        request = request.body(body.to_vec());
    }

    request
        .send()
        .await
        .map_err(|e| format!("请求 Codex 上游失败: {}", e))
}

async fn proxy_request_with_account_pool(
    request: &ParsedRequest,
    collection: &CodexLocalAccessCollection,
) -> Result<ProxyDispatchSuccess, ProxyDispatchError> {
    if collection.account_ids.is_empty() {
        return Err(ProxyDispatchError {
            status: 503,
            message: "本地接入集合暂无账号".to_string(),
            account_id: None,
            account_email: None,
        });
    }

    let upstream_target =
        resolve_upstream_target(&request.target).map_err(|err| ProxyDispatchError {
            status: 400,
            message: err,
            account_id: None,
            account_email: None,
        })?;
    let routing_hint = build_request_routing_hint(request);
    let total = collection.account_ids.len();
    let max_credential_attempts = total.min(MAX_RETRY_CREDENTIALS_PER_REQUEST).max(1);
    let affinity_account_id = match routing_hint.previous_response_id.as_deref() {
        Some(previous_response_id) => resolve_affinity_account(previous_response_id).await,
        None => None,
    };
    let mut last_status = 503u16;
    let mut last_error = "本地接入集合暂无可用账号".to_string();
    let mut last_account_id: Option<String> = None;
    let mut last_account_email: Option<String> = None;
    let mut attempts = 0usize;
    let mut retry_round = 0usize;
    let mut earliest_cooldown_wait: Option<Duration>;

    loop {
        let start = GATEWAY_ROUND_ROBIN_CURSOR.fetch_add(1, Ordering::Relaxed);
        let ordered_account_ids = build_ordered_account_ids(
            &collection.account_ids,
            start,
            affinity_account_id.as_deref(),
        );
        let strategy_account_ids = pin_account_to_front(
            apply_routing_strategy(&ordered_account_ids, collection.routing_strategy),
            affinity_account_id.as_deref(),
        );
        let mut attempted_in_round = false;
        let mut round_cooldown_wait: Option<Duration> = None;

        for account_id in strategy_account_ids {
            if attempts >= max_credential_attempts {
                break;
            }

            if let Some(wait) = get_model_cooldown_wait(&account_id, &routing_hint.model_key).await
            {
                round_cooldown_wait = Some(match round_cooldown_wait {
                    Some(current) if current <= wait => current,
                    _ => wait,
                });
                continue;
            }

            attempted_in_round = true;
            attempts += 1;

            let account = match codex_account::prepare_account_for_injection(&account_id).await {
                Ok(account) => account,
                Err(err) => {
                    last_error = err;
                    continue;
                }
            };

            if account.is_api_key_auth() {
                last_error = "API Key 账号不支持加入本地接入".to_string();
                continue;
            }
            if is_free_plan_type(account.plan_type.as_deref()) {
                last_error = "Free 账号不支持加入本地接入".to_string();
                continue;
            }

            last_account_id = Some(account.id.clone());
            last_account_email = Some(account.email.clone());

            let first_response = send_upstream_request(
                &request.method,
                &upstream_target,
                &request.headers,
                &request.body,
                &account,
            )
            .await;

            let mut response = match first_response {
                Ok(response) => response,
                Err(err) => {
                    last_error = err;
                    continue;
                }
            };

            if response.status() == StatusCode::UNAUTHORIZED {
                match force_refresh_gateway_account(&account_id).await {
                    Ok(()) => {
                        let refreshed_account = match codex_account::load_account(&account_id) {
                            Some(account) => account,
                            None => {
                                last_error = format!("账号不存在: {}", account_id);
                                continue;
                            }
                        };

                        response = match send_upstream_request(
                            &request.method,
                            &upstream_target,
                            &request.headers,
                            &request.body,
                            &refreshed_account,
                        )
                        .await
                        {
                            Ok(response) => response,
                            Err(err) => {
                                last_error = err;
                                continue;
                            }
                        };

                        if response.status() == StatusCode::UNAUTHORIZED {
                            last_status = StatusCode::UNAUTHORIZED.as_u16();
                            last_error = format!("账号 {} 鉴权失败", refreshed_account.email);
                            continue;
                        }
                    }
                    Err(err) => {
                        last_error = err;
                        continue;
                    }
                }
            }

            if response.status().is_success() {
                clear_model_cooldown(&account.id, &routing_hint.model_key).await;
                return Ok(ProxyDispatchSuccess {
                    upstream: response,
                    account_id: account.id.clone(),
                    account_email: account.email.clone(),
                });
            }

            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let message = summarize_upstream_error(status, &body);

            if let Some(retry_after) = parse_codex_retry_after(status, &body) {
                set_model_cooldown(&account.id, &routing_hint.model_key, retry_after).await;
                round_cooldown_wait = Some(match round_cooldown_wait {
                    Some(current) if current <= retry_after => current,
                    _ => retry_after,
                });
            }

            if should_try_next_account(status, &body) {
                last_status = status.as_u16();
                last_error = format!("账号 {} 当前不可用，已尝试轮转: {}", account.email, message);
                continue;
            }

            return Err(ProxyDispatchError {
                status: status.as_u16(),
                message,
                account_id: Some(account.id.clone()),
                account_email: Some(account.email.clone()),
            });
        }

        earliest_cooldown_wait = round_cooldown_wait;
        let Some(wait) = earliest_cooldown_wait else {
            break;
        };
        if attempts >= max_credential_attempts
            || retry_round >= MAX_REQUEST_RETRY_ATTEMPTS
            || wait > MAX_REQUEST_RETRY_WAIT
        {
            if !attempted_in_round {
                return Err(ProxyDispatchError {
                    status: StatusCode::TOO_MANY_REQUESTS.as_u16(),
                    message: build_cooldown_unavailable_message(&routing_hint.model_key, wait),
                    account_id: affinity_account_id.clone(),
                    account_email: None,
                });
            }
            break;
        }

        tokio::time::sleep(wait).await;
        retry_round += 1;
    }

    Err(ProxyDispatchError {
        status: if last_status == 503 {
            earliest_cooldown_wait
                .map(|_| StatusCode::TOO_MANY_REQUESTS.as_u16())
                .unwrap_or(last_status)
        } else {
            last_status
        },
        message: if matches!(last_status, 429 | 503) {
            earliest_cooldown_wait
                .map(|wait| build_cooldown_unavailable_message(&routing_hint.model_key, wait))
                .unwrap_or(last_error)
        } else {
            last_error
        },
        account_id: last_account_id,
        account_email: last_account_email,
    })
}

async fn handle_connection(mut stream: TcpStream) -> Result<(), String> {
    let raw_request = read_http_request(&mut stream).await?;
    let mut parsed = parse_http_request(&raw_request)?;

    if parsed.method.eq_ignore_ascii_case("OPTIONS") {
        stream
            .write_all(&options_response())
            .await
            .map_err(|e| format!("写入 OPTIONS 响应失败: {}", e))?;
        return Ok(());
    }

    if !parsed.method.eq_ignore_ascii_case("GET") && !parsed.method.eq_ignore_ascii_case("POST") {
        let response = json_response(
            405,
            "Method Not Allowed",
            &json!({ "error": "Only GET and POST are allowed" }),
        );
        stream
            .write_all(&response)
            .await
            .map_err(|e| format!("写入错误响应失败: {}", e))?;
        return Ok(());
    }

    parsed.target = normalize_proxy_target(&parsed.target)?;
    if !parsed.target.starts_with("/v1/") {
        let response = json_response(404, "Not Found", &json!({ "error": "Not Found" }));
        stream
            .write_all(&response)
            .await
            .map_err(|e| format!("写入错误响应失败: {}", e))?;
        return Ok(());
    }

    let Some(api_key) = extract_local_api_key(&parsed.headers) else {
        let response = json_response(
            401,
            "Unauthorized",
            &json!({ "error": "缺少 Authorization Bearer 或 X-API-Key" }),
        );
        stream
            .write_all(&response)
            .await
            .map_err(|e| format!("写入错误响应失败: {}", e))?;
        return Ok(());
    };

    let state = {
        let runtime = gateway_runtime().lock().await;
        build_state_snapshot(&runtime)
    };
    let Some(collection) = state.collection else {
        let response = json_response(
            503,
            "Service Unavailable",
            &json!({ "error": "本地接入集合尚未创建" }),
        );
        stream
            .write_all(&response)
            .await
            .map_err(|e| format!("写入错误响应失败: {}", e))?;
        return Ok(());
    };

    if !collection.enabled || !state.running {
        let response = json_response(
            503,
            "Service Unavailable",
            &json!({ "error": "本地接入服务未启用" }),
        );
        stream
            .write_all(&response)
            .await
            .map_err(|e| format!("写入错误响应失败: {}", e))?;
        return Ok(());
    }

    if api_key != collection.api_key {
        let response = json_response(401, "Unauthorized", &json!({ "error": "本地访问秘钥无效" }));
        stream
            .write_all(&response)
            .await
            .map_err(|e| format!("写入错误响应失败: {}", e))?;
        return Ok(());
    }

    if is_local_models_request(&parsed.target) {
        if collection.account_ids.is_empty() {
            let response = json_response(
                503,
                "Service Unavailable",
                &json!({ "error": "本地接入集合暂无账号" }),
            );
            stream
                .write_all(&response)
                .await
                .map_err(|e| format!("写入错误响应失败: {}", e))?;
            return Ok(());
        }

        let response = json_response(200, "OK", &build_local_models_response());
        stream
            .write_all(&response)
            .await
            .map_err(|e| format!("写入模型响应失败: {}", e))?;
        return Ok(());
    }

    let started_at = Instant::now();
    let request_is_stream = is_stream_request(&parsed.headers, &parsed.body);

    match proxy_request_with_account_pool(&parsed, &collection).await {
        Ok(success) => {
            let response_capture =
                write_upstream_response(&mut stream, success.upstream, request_is_stream).await?;
            if let Some(response_id) = response_capture.response_id.as_deref() {
                bind_response_affinity(response_id, &success.account_id).await;
            }
            let latency_ms = started_at.elapsed().as_millis() as u64;
            if let Err(err) = record_request_stats(
                Some(success.account_id.as_str()),
                Some(success.account_email.as_str()),
                true,
                latency_ms,
                response_capture.usage,
            )
            .await
            {
                logger::log_warn(&format!("[CodexLocalAccess] 写入请求统计失败: {}", err));
            }
            Ok(())
        }
        Err(error) => {
            let ProxyDispatchError {
                status,
                message,
                account_id,
                account_email,
            } = error;
            let status_text = match status {
                400 => "Bad Request",
                401 => "Unauthorized",
                404 => "Not Found",
                405 => "Method Not Allowed",
                429 => "Too Many Requests",
                502 => "Bad Gateway",
                _ => "Service Unavailable",
            };
            let response = json_response(status, status_text, &json!({ "error": message }));
            let write_result = stream
                .write_all(&response)
                .await
                .map_err(|e| format!("写入错误响应失败: {}", e));
            let latency_ms = started_at.elapsed().as_millis() as u64;
            if let Err(err) = record_request_stats(
                account_id.as_deref(),
                account_email.as_deref(),
                false,
                latency_ms,
                None,
            )
            .await
            {
                logger::log_warn(&format!("[CodexLocalAccess] 写入失败统计失败: {}", err));
            }
            write_result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_ordered_account_ids, build_request_routing_hint, extract_usage_capture,
        parse_codex_retry_after, should_treat_response_as_stream, ParsedRequest,
        ResponseUsageCollector,
    };
    use reqwest::StatusCode;
    use serde_json::json;
    use std::collections::HashMap;
    use tokio::time::Duration;

    #[test]
    fn extracts_usage_from_codex_response_completed_payload() {
        let payload = json!({
            "type": "response.completed",
            "response": {
                "usage": {
                    "input_tokens": 16,
                    "input_tokens_details": {
                        "cached_tokens": 3
                    },
                    "output_tokens": 5,
                    "output_tokens_details": {
                        "reasoning_tokens": 2
                    },
                    "total_tokens": 21
                }
            }
        });

        let usage = extract_usage_capture(&payload).expect("usage should be parsed");
        assert_eq!(usage.input_tokens, 16);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cached_tokens, 3);
        assert_eq!(usage.reasoning_tokens, 2);
        assert_eq!(usage.total_tokens, 21);
    }

    #[test]
    fn extracts_usage_from_openai_prompt_and_completion_details() {
        let payload = json!({
            "usage": {
                "prompt_tokens": 8,
                "prompt_tokens_details": {
                    "cached_tokens": 1
                },
                "completion_tokens": 4,
                "completion_tokens_details": {
                    "reasoning_tokens": 2
                }
            }
        });

        let usage = extract_usage_capture(&payload).expect("usage should be parsed");
        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.output_tokens, 4);
        assert_eq!(usage.cached_tokens, 1);
        assert_eq!(usage.reasoning_tokens, 2);
        assert_eq!(usage.total_tokens, 14);
    }

    #[test]
    fn parses_sse_usage_when_request_is_stream_even_if_content_type_is_json() {
        assert!(should_treat_response_as_stream(
            "application/json; charset=utf-8",
            true
        ));

        let mut collector = ResponseUsageCollector::new(true);
        collector.feed(
            br#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_123","usage":{"input_tokens":16,"input_tokens_details":{"cached_tokens":0},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":21}}}

"#,
        );

        let capture = collector.finish();
        let usage = capture.usage.expect("stream usage should be parsed");
        assert_eq!(usage.input_tokens, 16);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, 21);
        assert_eq!(capture.response_id.as_deref(), Some("resp_123"));
    }

    #[test]
    fn parses_codex_retry_after_from_usage_limit_payload() {
        let wait = parse_codex_retry_after(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"usage_limit_reached","resets_in_seconds":12}}"#,
        )
        .expect("retry after should be parsed");

        assert_eq!(wait, Duration::from_secs(12));
    }

    #[test]
    fn prefers_affinity_account_before_round_robin_order() {
        let ordered = build_ordered_account_ids(
            &[
                "acc-a".to_string(),
                "acc-b".to_string(),
                "acc-c".to_string(),
            ],
            1,
            Some("acc-c"),
        );

        assert_eq!(ordered, vec!["acc-c", "acc-b", "acc-a"]);
    }

    #[test]
    fn builds_routing_hint_from_previous_response_id_and_model() {
        let request = ParsedRequest {
            method: "POST".to_string(),
            target: "/v1/responses".to_string(),
            headers: HashMap::new(),
            body: br#"{"model":"GPT-5.4-mini","previous_response_id":"resp_prev"}"#.to_vec(),
        };

        let hint = build_request_routing_hint(&request);
        assert_eq!(hint.model_key, "gpt-5.4-mini");
        assert_eq!(hint.previous_response_id.as_deref(), Some("resp_prev"));
    }
}
