use crate::modules;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

const HISTORY_FILE: &str = "antigravity_switch_history.json";
const MAX_HISTORY_ITEMS: usize = 200;

static HISTORY_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAutoSwitchHitGroup {
    pub group_id: String,
    pub group_name: String,
    pub percentage: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAutoSwitchReason {
    pub rule: String,
    pub threshold: i32,
    pub scope_mode: String,
    #[serde(default)]
    pub selected_group_ids: Vec<String>,
    #[serde(default)]
    pub selected_group_names: Vec<String>,
    #[serde(default)]
    pub hit_groups: Vec<AntigravityAutoSwitchHitGroup>,
    pub candidate_count: usize,
    pub selected_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravitySwitchHistoryItem {
    pub id: String,
    pub timestamp: i64,
    pub account_id: String,
    pub target_email: String,
    #[serde(default = "default_trigger_type")]
    pub trigger_type: String,
    #[serde(default = "default_trigger_source")]
    pub trigger_source: String,
    pub local_ok: bool,
    pub seamless_ok: bool,
    pub success: bool,
    pub local_duration_ms: u64,
    pub seamless_duration_ms: Option<u64>,
    pub total_duration_ms: u64,
    pub error_stage: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub seamless_effective_mode: Option<String>,
    pub seamless_from_email: Option<String>,
    pub seamless_to_email: Option<String>,
    pub seamless_execution_id: Option<String>,
    pub seamless_finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_switch_reason: Option<AntigravityAutoSwitchReason>,
}

fn default_trigger_type() -> String {
    "manual".to_string()
}

fn default_trigger_source() -> String {
    "tools.account.switch".to_string()
}

fn history_path() -> Result<PathBuf, String> {
    let data_dir = modules::account::get_data_dir()?;
    Ok(data_dir.join(HISTORY_FILE))
}

pub fn load_history() -> Result<Vec<AntigravitySwitchHistoryItem>, String> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content =
        fs::read_to_string(&path).map_err(|e| format!("读取 Antigravity 切号记录失败: {}", e))?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    serde_json::from_str::<Vec<AntigravitySwitchHistoryItem>>(&content)
        .map_err(|e| format!("解析 Antigravity 切号记录失败: {}", e))
}

fn save_history(items: &[AntigravitySwitchHistoryItem]) -> Result<(), String> {
    let path = history_path()?;
    let data_dir = modules::account::get_data_dir()?;
    let temp_path = data_dir.join(format!("{}.tmp", HISTORY_FILE));
    let content = serde_json::to_string_pretty(items)
        .map_err(|e| format!("序列化 Antigravity 切号记录失败: {}", e))?;

    fs::write(&temp_path, content).map_err(|e| format!("写入临时切号记录文件失败: {}", e))?;
    fs::rename(temp_path, path).map_err(|e| format!("替换切号记录文件失败: {}", e))
}

pub fn add_history_item(item: AntigravitySwitchHistoryItem) -> Result<(), String> {
    let _lock = HISTORY_LOCK.lock().map_err(|_| "获取切号记录锁失败")?;
    let mut existing = load_history().unwrap_or_default();

    existing.retain(|x| x.id != item.id);
    existing.push(item);
    existing.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    existing.truncate(MAX_HISTORY_ITEMS);

    save_history(&existing)
}

pub fn clear_history() -> Result<(), String> {
    let _lock = HISTORY_LOCK.lock().map_err(|_| "获取切号记录锁失败")?;
    save_history(&[])
}
