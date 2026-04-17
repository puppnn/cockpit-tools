use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use lazy_static::lazy_static;
use regex::Regex;
use rusqlite::{types::Value, Connection, OpenFlags};
use serde::Serialize;
use serde_json::{json, Map, Value as JsonValue};
use url::Url;

use crate::modules;

const DEFAULT_INSTANCE_ID: &str = "__default__";
const DEFAULT_INSTANCE_NAME: &str = "Default";
const STATE_DB_FILE: &str = "state_5.sqlite";
const SESSION_INDEX_FILE: &str = "session_index.jsonl";
const SESSION_FAVORITE_ROOT_DIR: &str = "cockpit-tools-session-favorites";

lazy_static! {
    static ref SESSION_ID_REGEX: Regex =
        Regex::new(r"([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})")
            .expect("invalid session id regex");
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionViewerLocation {
    pub instance_id: String,
    pub instance_name: String,
    pub running: bool,
    pub session_path: Option<String>,
    pub updated_at: Option<i64>,
    pub created_at: Option<i64>,
    pub cwd: String,
    pub model_provider: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionViewerRecord {
    pub session_id: String,
    pub title: String,
    pub cwd: String,
    pub updated_at: Option<i64>,
    pub created_at: Option<i64>,
    pub model_provider: String,
    pub session_path: Option<String>,
    pub location_count: usize,
    pub is_favorite: bool,
    pub locations: Vec<CodexSessionViewerLocation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexTimelineEvent {
    pub id: String,
    pub timestamp: String,
    pub kind: String,
    pub role: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    pub raw: String,
    pub call_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionTimeline {
    pub session: CodexSessionViewerRecord,
    pub events: Vec<CodexTimelineEvent>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionTitleUpdateResult {
    pub session_id: String,
    pub title: String,
    pub matched_instance_count: usize,
    pub rollout_file_updated_count: usize,
    pub session_index_updated_count: usize,
    pub sqlite_updated_count: usize,
    pub backup_paths: Vec<String>,
    pub warnings: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionFavoriteResult {
    pub session_id: String,
    pub matched_instance_count: usize,
    pub backup_paths: Vec<String>,
    pub warnings: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone)]
struct CodexSyncInstance {
    id: String,
    name: String,
    data_dir: PathBuf,
    last_pid: Option<u32>,
}

#[derive(Debug, Clone)]
struct SessionIndexEntry {
    id: String,
    title: String,
    updated_at: Option<i64>,
    raw: JsonValue,
}

#[derive(Debug, Clone)]
struct ThreadRowData {
    columns: Vec<String>,
    values: Vec<Value>,
}

#[derive(Debug, Clone)]
struct ThreadDbRecord {
    id: String,
    title: String,
    cwd: String,
    model_provider: String,
    created_at: Option<i64>,
    updated_at: Option<i64>,
    rollout_path: Option<PathBuf>,
    row_data: ThreadRowData,
}

#[derive(Debug, Clone)]
struct SessionFileSummary {
    title: String,
    cwd: String,
    model_provider: String,
    created_at: Option<i64>,
    updated_at: Option<i64>,
}

#[derive(Debug, Clone)]
struct SessionSnapshot {
    id: String,
    title: String,
    cwd: String,
    updated_at: Option<i64>,
    created_at: Option<i64>,
    model_provider: String,
    rollout_path: Option<PathBuf>,
    row_data: Option<ThreadRowData>,
    session_index_entry: Option<JsonValue>,
}

#[derive(Debug, Clone)]
struct SessionMatch {
    instance: CodexSyncInstance,
    running: bool,
    snapshot: SessionSnapshot,
}

pub fn list_sessions_across_instances() -> Result<Vec<CodexSessionViewerRecord>, String> {
    let matches = collect_session_matches(None)?;
    let mut session_map = HashMap::<String, CodexSessionViewerRecord>::new();

    for item in matches {
        let entry = session_map
            .entry(item.snapshot.id.to_lowercase())
            .or_insert_with(|| build_record_from_snapshot(&item.snapshot));
        merge_snapshot_into_record(entry, &item.instance, item.running, &item.snapshot);
    }

    let mut sessions = session_map.into_values().collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .unwrap_or_default()
            .cmp(&left.updated_at.unwrap_or_default())
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(sessions)
}

pub fn get_session_timeline(
    session_id: String,
    preferred_instance_id: Option<String>,
) -> Result<CodexSessionTimeline, String> {
    let matches = collect_session_matches(Some(session_id.as_str()))?;
    if matches.is_empty() {
        return Err(format!("Session not found: {}", session_id));
    }

    let mut record = build_record_from_snapshot(&matches[0].snapshot);
    for item in &matches {
        merge_snapshot_into_record(&mut record, &item.instance, item.running, &item.snapshot);
    }

    let preferred_instance = preferred_instance_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let selected = choose_session_match(&matches, preferred_instance)
        .ok_or_else(|| format!("No timeline source available for session: {}", session_id))?;

    let mut warnings = Vec::new();
    let Some(path) = selected.snapshot.rollout_path.as_ref() else {
        warnings.push("No session file found. Only metadata is available.".to_string());
        return Ok(CodexSessionTimeline {
            session: record,
            events: Vec::new(),
            warnings,
        });
    };
    if !path.exists() {
        warnings.push(format!("Session file is missing: {}", path.display()));
        return Ok(CodexSessionTimeline {
            session: record,
            events: Vec::new(),
            warnings,
        });
    }

    let (events, mut parse_warnings) = parse_timeline_from_file(path)?;
    warnings.append(&mut parse_warnings);
    Ok(CodexSessionTimeline {
        session: record,
        events,
        warnings,
    })
}

pub fn update_session_title(
    session_id: String,
    title: String,
) -> Result<CodexSessionTitleUpdateResult, String> {
    let next_title = title.trim();
    if next_title.is_empty() {
        return Err("Title cannot be empty.".to_string());
    }

    let now = Utc::now().to_rfc3339();
    let matches = collect_session_matches(Some(session_id.as_str()))?;
    if matches.is_empty() {
        return Err(format!("Session not found: {}", session_id));
    }

    let mut warnings = Vec::new();
    let mut rollout_file_updated_count = 0usize;
    let mut session_index_updated_count = 0usize;
    let mut sqlite_updated_count = 0usize;

    for item in &matches {
        match upsert_rollout_title(item.snapshot.rollout_path.as_deref(), &item.snapshot.id, next_title) {
            Ok(true) => rollout_file_updated_count += 1,
            Ok(false) => warnings.push(format!(
                "{}: session file update skipped because rollout file is missing.",
                item.instance.name
            )),
            Err(error) => warnings.push(format!(
                "{}: session file update failed: {}",
                item.instance.name, error
            )),
        }

        let index_path = item.instance.data_dir.join(SESSION_INDEX_FILE);
        match upsert_session_index(
            &index_path,
            &item.snapshot.id,
            next_title,
            &now,
            item.snapshot.session_index_entry.as_ref(),
        ) {
            Ok(true) => session_index_updated_count += 1,
            Ok(false) => {}
            Err(error) => warnings.push(format!(
                "{}: session_index update failed: {}",
                item.instance.name, error
            )),
        }

        match upsert_thread_title(&item.instance.data_dir, &item.snapshot, next_title, &now) {
            Ok(true) => sqlite_updated_count += 1,
            Ok(false) => {}
            Err(error) => warnings.push(format!(
                "{}: sqlite update failed: {}",
                item.instance.name, error
            )),
        }
    }

    Ok(CodexSessionTitleUpdateResult {
        session_id,
        title: next_title.to_string(),
        matched_instance_count: matches.len(),
        rollout_file_updated_count,
        session_index_updated_count,
        sqlite_updated_count,
        backup_paths: Vec::new(),
        warnings: warnings.clone(),
        message: if warnings.is_empty() {
            format!("Updated title in {} instance(s).", matches.len())
        } else {
            format!(
                "Updated title in {} instance(s), with {} warning(s).",
                matches.len(),
                warnings.len()
            )
        },
    })
}

pub fn favorite_session(session_id: String) -> Result<CodexSessionFavoriteResult, String> {
    let matches = collect_session_matches(Some(session_id.as_str()))?;
    if matches.is_empty() {
        return Err(format!("Session not found: {}", session_id));
    }

    let mut backup_paths = Vec::new();
    let mut warnings = Vec::new();

    for item in &matches {
        let index_path = item.instance.data_dir.join(SESSION_INDEX_FILE);
        let sqlite_path = item.instance.data_dir.join(STATE_DB_FILE);
        let mut file_paths = vec![index_path, sqlite_path.clone(), wal_path(&sqlite_path), shm_path(&sqlite_path)];
        if let Some(rollout_path) = item.snapshot.rollout_path.as_ref() {
            file_paths.push(rollout_path.clone());
        } else {
            warnings.push(format!(
                "{}: session file is missing, only index/sqlite snapshot was backed up.",
                item.instance.name
            ));
        }

        let favorite_dir = get_session_favorite_dir(&item.instance.id, &item.snapshot.id)?;
        backup_paths.extend(copy_files_to_dir(
            &favorite_dir,
            &file_paths,
            Some(build_favorite_manifest(item, &file_paths)?),
        )?);
    }

    Ok(CodexSessionFavoriteResult {
        session_id,
        matched_instance_count: matches.len(),
        backup_paths,
        warnings: warnings.clone(),
        message: if warnings.is_empty() {
            format!("Favorited session in {} instance(s).", matches.len())
        } else {
            format!(
                "Favorited session in {} instance(s), with {} warning(s).",
                matches.len(),
                warnings.len()
            )
        },
    })
}

pub fn unfavorite_session(session_id: String) -> Result<CodexSessionFavoriteResult, String> {
    let matches = collect_session_matches(Some(session_id.as_str()))?;
    if matches.is_empty() {
        return Err(format!("Session not found: {}", session_id));
    }

    let mut removed_paths = Vec::new();
    let mut warnings = Vec::new();

    for item in &matches {
        let favorite_dir = get_session_favorite_dir(&item.instance.id, &item.snapshot.id)?;
        if !favorite_dir.exists() {
            continue;
        }

        fs::remove_dir_all(&favorite_dir).map_err(|error| {
            format!(
                "Failed to remove favorite dir {}: {}",
                favorite_dir.display(),
                error
            )
        })?;
        removed_paths.push(favorite_dir.to_string_lossy().to_string());
    }

    if removed_paths.is_empty() {
        warnings.push("No existing favorite backup was found.".to_string());
    }

    Ok(CodexSessionFavoriteResult {
        session_id,
        matched_instance_count: matches.len(),
        backup_paths: removed_paths,
        warnings: warnings.clone(),
        message: if warnings.is_empty() {
            format!("Removed favorite backup in {} instance(s).", matches.len())
        } else {
            format!(
                "Removed favorite backup in {} instance(s), with {} warning(s).",
                matches.len(),
                warnings.len()
            )
        },
    })
}

fn collect_session_matches(session_id: Option<&str>) -> Result<Vec<SessionMatch>, String> {
    let instances = collect_instances()?;
    let process_entries = modules::process::collect_codex_process_entries();
    let target_id = session_id.map(|value| value.to_lowercase());
    let mut matches = Vec::new();

    for instance in instances {
        let running = is_instance_running(&instance, &process_entries);
        for snapshot in load_instance_sessions(&instance)? {
            if let Some(ref target) = target_id {
                if snapshot.id.to_lowercase() != *target {
                    continue;
                }
            }
            matches.push(SessionMatch {
                instance: instance.clone(),
                running,
                snapshot,
            });
        }
    }

    Ok(matches)
}

fn collect_instances() -> Result<Vec<CodexSyncInstance>, String> {
    let mut instances = Vec::new();
    let default_dir = modules::codex_instance::get_default_codex_home()?;
    let store = modules::codex_instance::load_instance_store()?;
    instances.push(CodexSyncInstance {
        id: DEFAULT_INSTANCE_ID.to_string(),
        name: DEFAULT_INSTANCE_NAME.to_string(),
        data_dir: default_dir,
        last_pid: store.default_settings.last_pid,
    });

    for instance in store.instances {
        let user_data_dir = instance.user_data_dir.trim();
        if user_data_dir.is_empty() {
            continue;
        }
        instances.push(CodexSyncInstance {
            id: instance.id,
            name: if instance.name.trim().is_empty() {
                "Instance".to_string()
            } else {
                instance.name
            },
            data_dir: PathBuf::from(user_data_dir),
            last_pid: instance.last_pid,
        });
    }

    Ok(instances)
}

fn is_instance_running(
    instance: &CodexSyncInstance,
    process_entries: &[(u32, Option<String>)],
) -> bool {
    let codex_home = if instance.id == DEFAULT_INSTANCE_ID {
        None
    } else {
        instance.data_dir.to_str()
    };
    modules::process::resolve_codex_pid_from_entries(instance.last_pid, codex_home, process_entries)
        .is_some()
}

fn build_record_from_snapshot(snapshot: &SessionSnapshot) -> CodexSessionViewerRecord {
    CodexSessionViewerRecord {
        session_id: snapshot.id.clone(),
        title: if snapshot.title.trim().is_empty() {
            snapshot.id.clone()
        } else {
            snapshot.title.clone()
        },
        cwd: snapshot.cwd.clone(),
        updated_at: snapshot.updated_at,
        created_at: snapshot.created_at,
        model_provider: snapshot.model_provider.clone(),
        session_path: snapshot
            .rollout_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        location_count: 0,
        is_favorite: false,
        locations: Vec::new(),
    }
}

fn merge_snapshot_into_record(
    record: &mut CodexSessionViewerRecord,
    instance: &CodexSyncInstance,
    running: bool,
    snapshot: &SessionSnapshot,
) {
    record.locations.push(CodexSessionViewerLocation {
        instance_id: instance.id.clone(),
        instance_name: instance.name.clone(),
        running,
        session_path: snapshot
            .rollout_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        updated_at: snapshot.updated_at,
        created_at: snapshot.created_at,
        cwd: snapshot.cwd.clone(),
        model_provider: snapshot.model_provider.clone(),
    });
    record.location_count = record.locations.len();
    record.is_favorite = record.is_favorite || is_session_favorited(&instance.id, &snapshot.id);
    record.locations.sort_by(|left, right| {
        right
            .running
            .cmp(&left.running)
            .then_with(|| right.updated_at.unwrap_or_default().cmp(&left.updated_at.unwrap_or_default()))
            .then_with(|| left.instance_name.cmp(&right.instance_name))
    });

    let promote = snapshot.updated_at.unwrap_or_default() > record.updated_at.unwrap_or_default()
        || (record.session_path.is_none() && snapshot.rollout_path.is_some())
        || (record.title.trim().is_empty() && !snapshot.title.trim().is_empty())
        || (record.cwd.trim().is_empty() && !snapshot.cwd.trim().is_empty())
        || (record.model_provider.trim().is_empty() && !snapshot.model_provider.trim().is_empty());

    if promote {
        if !snapshot.title.trim().is_empty() {
            record.title = snapshot.title.clone();
        }
        if !snapshot.cwd.trim().is_empty() {
            record.cwd = snapshot.cwd.clone();
        }
        if !snapshot.model_provider.trim().is_empty() {
            record.model_provider = snapshot.model_provider.clone();
        }
        if snapshot.rollout_path.is_some() {
            record.session_path = snapshot
                .rollout_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string());
        }
    }

    if snapshot.updated_at.unwrap_or_default() > record.updated_at.unwrap_or_default() {
        record.updated_at = snapshot.updated_at;
    }
    match (record.created_at, snapshot.created_at) {
        (None, value) => record.created_at = value,
        (Some(current), Some(candidate)) if candidate < current => record.created_at = Some(candidate),
        _ => {}
    }
}

fn choose_session_match<'a>(
    matches: &'a [SessionMatch],
    preferred_instance_id: Option<&str>,
) -> Option<&'a SessionMatch> {
    if let Some(instance_id) = preferred_instance_id {
        if let Some(item) = matches.iter().find(|item| item.instance.id == instance_id) {
            if item
                .snapshot
                .rollout_path
                .as_ref()
                .map(|path| path.exists())
                .unwrap_or(false)
            {
                return Some(item);
            }
        }
    }

    matches.iter().max_by(|left, right| {
        let left_has_file = left
            .snapshot
            .rollout_path
            .as_ref()
            .map(|path| path.exists())
            .unwrap_or(false);
        let right_has_file = right
            .snapshot
            .rollout_path
            .as_ref()
            .map(|path| path.exists())
            .unwrap_or(false);
        left_has_file
            .cmp(&right_has_file)
            .then_with(|| left.running.cmp(&right.running))
            .then_with(|| left.snapshot.updated_at.unwrap_or_default().cmp(&right.snapshot.updated_at.unwrap_or_default()))
    })
}

fn load_instance_sessions(instance: &CodexSyncInstance) -> Result<Vec<SessionSnapshot>, String> {
    let index_map = read_session_index_map(&instance.data_dir)?;
    let thread_rows = read_thread_rows(&instance.data_dir)?;
    let sessions_root = instance.data_dir.join("sessions");
    let mut records = HashMap::<String, SessionSnapshot>::new();

    for session_path in enumerate_session_files(&sessions_root)? {
        let Some(session_id) = extract_session_id(&session_path) else {
            continue;
        };
        let file_summary = read_session_file_summary(&session_path, &session_id)?;
        let mut snapshot = SessionSnapshot {
            id: session_id.clone(),
            title: first_non_empty(&[
                index_map
                    .get(&session_id.to_lowercase())
                    .map(|item| item.title.as_str()),
                Some(file_summary.title.as_str()),
            ])
            .unwrap_or(&session_id)
            .to_string(),
            cwd: file_summary.cwd.clone(),
            updated_at: file_summary.updated_at,
            created_at: file_summary.created_at,
            model_provider: file_summary.model_provider.clone(),
            rollout_path: Some(session_path.clone()),
            row_data: None,
            session_index_entry: index_map
                .get(&session_id.to_lowercase())
                .map(|item| item.raw.clone()),
        };

        if let Some(thread_row) = thread_rows.get(&session_id.to_lowercase()) {
            apply_thread_row_to_snapshot(&mut snapshot, thread_row);
        }

        records.insert(session_id.to_lowercase(), snapshot);
    }

    for (key, index_entry) in &index_map {
        let record = records.entry(key.clone()).or_insert_with(|| SessionSnapshot {
            id: index_entry.id.clone(),
            title: if index_entry.title.trim().is_empty() {
                index_entry.id.clone()
            } else {
                index_entry.title.clone()
            },
            cwd: String::new(),
            updated_at: index_entry.updated_at,
            created_at: index_entry.updated_at,
            model_provider: String::new(),
            rollout_path: None,
            row_data: None,
            session_index_entry: Some(index_entry.raw.clone()),
        });
        record.session_index_entry = Some(index_entry.raw.clone());
        if record.title.trim().is_empty() && !index_entry.title.trim().is_empty() {
            record.title = index_entry.title.clone();
        }
        if record.updated_at.is_none() {
            record.updated_at = index_entry.updated_at;
        }
        if record.created_at.is_none() {
            record.created_at = index_entry.updated_at;
        }
    }

    for (key, thread_row) in &thread_rows {
        let record = records.entry(key.clone()).or_insert_with(|| SessionSnapshot {
            id: thread_row.id.clone(),
            title: if thread_row.title.trim().is_empty() {
                thread_row.id.clone()
            } else {
                thread_row.title.clone()
            },
            cwd: thread_row.cwd.clone(),
            updated_at: thread_row.updated_at,
            created_at: thread_row.created_at,
            model_provider: thread_row.model_provider.clone(),
            rollout_path: thread_row.rollout_path.clone(),
            row_data: Some(thread_row.row_data.clone()),
            session_index_entry: index_map.get(key).map(|item| item.raw.clone()),
        });
        apply_thread_row_to_snapshot(record, thread_row);
    }

    let mut snapshots = records.into_values().collect::<Vec<_>>();
    snapshots.sort_by(|left, right| {
        right
            .updated_at
            .unwrap_or_default()
            .cmp(&left.updated_at.unwrap_or_default())
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(snapshots)
}

fn read_session_index_map(root_dir: &Path) -> Result<HashMap<String, SessionIndexEntry>, String> {
    let path = root_dir.join(SESSION_INDEX_FILE);
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content = fs::read_to_string(&path)
        .map_err(|error| format!("Failed to read {}: {}", path.display(), error))?;
    let mut entries = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<JsonValue>(trimmed) else {
            continue;
        };
        let Some(id) = parsed.get("id").and_then(JsonValue::as_str) else {
            continue;
        };
        let title = parsed
            .get("thread_name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let updated_at = parsed
            .get("updated_at")
            .and_then(JsonValue::as_str)
            .and_then(parse_rfc3339_to_timestamp);
        entries.insert(
            id.to_lowercase(),
            SessionIndexEntry {
                id: id.to_string(),
                title,
                updated_at,
                raw: parsed,
            },
        );
    }

    Ok(entries)
}

fn read_thread_rows(root_dir: &Path) -> Result<HashMap<String, ThreadDbRecord>, String> {
    let db_path = root_dir.join(STATE_DB_FILE);
    if !db_path.exists() {
        return Ok(HashMap::new());
    }

    let connection = open_readonly_connection(&db_path)?;
    let columns = read_thread_columns(&connection)?;
    let select_columns = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!("SELECT {} FROM threads", select_columns);
    let mut statement = connection
        .prepare(&query)
        .map_err(|error| format!("Failed to query {}: {}", db_path.display(), error))?;
    let mut rows = statement
        .query([])
        .map_err(|error| format!("Failed to iterate {}: {}", db_path.display(), error))?;

    let mut result = HashMap::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| format!("Failed to read thread row in {}: {}", db_path.display(), error))?
    {
        let mut values = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            values.push(
                row.get::<usize, Value>(index)
                    .map_err(|error| format!("Failed to parse row in {}: {}", db_path.display(), error))?,
            );
        }

        let row_data = ThreadRowData {
            columns: columns.clone(),
            values,
        };
        let Some(id) = row_data.get_text("id") else {
            continue;
        };
        let title = row_data
            .get_text("title")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        let cwd = row_data
            .get_text("cwd")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default();
        let model_provider = row_data
            .get_text("model_provider")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default();
        let rollout_path = row_data
            .get_text("rollout_path")
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from);

        result.insert(
            id.to_lowercase(),
            ThreadDbRecord {
                id,
                title,
                cwd,
                model_provider,
                created_at: row_data.get_i64("created_at"),
                updated_at: row_data.get_i64("updated_at"),
                rollout_path,
                row_data,
            },
        );
    }

    Ok(result)
}

fn enumerate_session_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current)
            .map_err(|error| format!("Failed to read sessions directory {}: {}", current.display(), error))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("Failed to iterate {}: {}", current.display(), error))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Failed to inspect {}: {}", path.display(), error))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if file_type.is_file()
                && path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.eq_ignore_ascii_case("jsonl"))
                    .unwrap_or(false)
            {
                files.push(path);
            }
        }
    }

    Ok(files)
}

fn extract_session_id(path: &Path) -> Option<String> {
    if let Some(file_name) = path.file_name().and_then(|value| value.to_str()) {
        if let Some(capture) = SESSION_ID_REGEX.captures(file_name) {
            return capture.get(1).map(|item| item.as_str().to_string());
        }
    }

    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(5) {
        let line = line.ok()?;
        let parsed = serde_json::from_str::<JsonValue>(&line).ok()?;
        let payload = parsed.get("payload")?.as_object()?;
        let id = payload.get("id")?.as_str()?.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }

    None
}

fn read_session_file_summary(path: &Path, session_id: &str) -> Result<SessionFileSummary, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Failed to read {}: {}", path.display(), error))?;
    let created_at = system_time_to_timestamp(metadata.created().ok())
        .or_else(|| system_time_to_timestamp(metadata.modified().ok()));
    let updated_at = system_time_to_timestamp(metadata.modified().ok());

    let file = fs::File::open(path)
        .map_err(|error| format!("Failed to open {}: {}", path.display(), error))?;
    let reader = BufReader::new(file);

    let mut title = String::new();
    let mut cwd = String::new();
    let mut model_provider = String::new();
    let mut created_hint = None;

    for (index, line) in reader.lines().enumerate() {
        if index > 160 && !title.is_empty() && !cwd.is_empty() && !model_provider.is_empty() {
            break;
        }
        let line = match line {
            Ok(value) => value,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<JsonValue>(trimmed) else {
            continue;
        };
        let Some(payload) = parsed.get("payload").and_then(JsonValue::as_object) else {
            continue;
        };
        let payload_type = payload
            .get("type")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        match payload_type {
            "session_meta" => {
                if title.is_empty() {
                    title = first_non_empty(&[
                        payload.get("thread_name").and_then(JsonValue::as_str),
                        payload.get("title").and_then(JsonValue::as_str),
                    ])
                    .unwrap_or_default()
                    .to_string();
                }
                if cwd.is_empty() {
                    cwd = payload
                        .get("cwd")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                }
                if model_provider.is_empty() {
                    model_provider = payload
                        .get("model_provider")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                }
                if created_hint.is_none() {
                    created_hint = payload
                        .get("timestamp")
                        .and_then(JsonValue::as_str)
                        .and_then(parse_rfc3339_to_timestamp);
                }
            }
            "user_message" => {
                if title.is_empty() {
                    if let Some(message) = payload.get("message").and_then(JsonValue::as_str) {
                        title = normalize_title(message);
                    }
                }
            }
            "message" => {
                if title.is_empty()
                    && payload
                        .get("role")
                        .and_then(JsonValue::as_str)
                        .map(|value| value == "user")
                        .unwrap_or(false)
                {
                    title = normalize_title(&extract_message_content(payload));
                }
            }
            _ => {}
        }

        if !title.is_empty() && !cwd.is_empty() && !model_provider.is_empty() {
            break;
        }
    }

    Ok(SessionFileSummary {
        title: if title.is_empty() {
            session_id.to_string()
        } else {
            title
        },
        cwd,
        model_provider,
        created_at: created_hint.or(created_at),
        updated_at,
    })
}

fn apply_thread_row_to_snapshot(snapshot: &mut SessionSnapshot, thread_row: &ThreadDbRecord) {
    snapshot.row_data = Some(thread_row.row_data.clone());
    if !thread_row.title.trim().is_empty() {
        snapshot.title = thread_row.title.clone();
    }
    if !thread_row.cwd.trim().is_empty() {
        snapshot.cwd = thread_row.cwd.clone();
    }
    if !thread_row.model_provider.trim().is_empty() {
        snapshot.model_provider = thread_row.model_provider.clone();
    }
    if thread_row.created_at.is_some() {
        snapshot.created_at = thread_row.created_at;
    }
    if thread_row.updated_at.is_some() {
        snapshot.updated_at = thread_row.updated_at;
    }
    if snapshot.rollout_path.is_none() {
        snapshot.rollout_path = thread_row.rollout_path.clone();
    }
}

fn parse_timeline_from_file(path: &Path) -> Result<(Vec<CodexTimelineEvent>, Vec<String>), String> {
    let file = fs::File::open(path)
        .map_err(|error| format!("Failed to open {}: {}", path.display(), error))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut warnings = Vec::new();

    for (index, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(value) => value,
            Err(error) => {
                warnings.push(format!("Line {} could not be read: {}", index + 1, error));
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed = match serde_json::from_str::<JsonValue>(trimmed) {
            Ok(value) => value,
            Err(error) => {
                warnings.push(format!("Line {} is invalid JSON: {}", index + 1, error));
                continue;
            }
        };
        events.push(to_timeline_event(index, &parsed));
    }

    Ok((events, warnings))
}

fn to_timeline_event(index: usize, row: &JsonValue) -> CodexTimelineEvent {
    let timestamp = row
        .get("timestamp")
        .and_then(JsonValue::as_str)
        .map(normalize_timestamp_string)
        .unwrap_or_default();
    let raw = serde_json::to_string_pretty(row).unwrap_or_else(|_| row.to_string());
    let payload = row.get("payload").and_then(JsonValue::as_object);
    let payload_type = payload
        .and_then(|value| value.get("type"))
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let top_level_type = row.get("type").and_then(JsonValue::as_str).unwrap_or_default();

    let mut event = CodexTimelineEvent {
        id: format!("line-{}", index),
        timestamp,
        kind: "unknown".to_string(),
        role: top_level_type.to_string(),
        title: if payload_type.is_empty() {
            top_level_type.to_string()
        } else {
            payload_type.to_string()
        },
        summary: summarize_text(&raw, 180),
        body: raw.clone(),
        raw,
        call_id: String::new(),
        status: String::new(),
    };

    let Some(payload) = payload else {
        return event;
    };

    match payload_type {
        "session_meta" => {
            let cwd = payload.get("cwd").and_then(JsonValue::as_str).unwrap_or_default();
            let provider = payload
                .get("model_provider")
                .and_then(JsonValue::as_str)
                .unwrap_or_default();
            event.kind = "session_meta".to_string();
            event.role = "system".to_string();
            event.title = "Session Meta".to_string();
            event.summary = summarize_text(&format!("cwd: {} | provider: {}", cwd, provider), 180);
            event.body = stringify_map(payload);
        }
        "turn_context" => {
            let summary = payload
                .get("summary")
                .map(stringify_json_or_string)
                .unwrap_or_default();
            event.kind = "turn_context".to_string();
            event.role = "system".to_string();
            event.title = "Turn Context".to_string();
            event.summary = if summary.is_empty() {
                summarize_text(&stringify_map(payload), 180)
            } else {
                summarize_text(&summary, 180)
            };
            event.body = stringify_map(payload);
        }
        "message" => {
            let role = payload.get("role").and_then(JsonValue::as_str).unwrap_or_default();
            let body = extract_message_content(payload);
            event.role = role.to_string();
            event.kind = match role {
                "user" => "user_message",
                "assistant" => "assistant_message",
                "developer" => "developer_message",
                _ => "unknown",
            }
            .to_string();
            event.title = match role {
                "user" => "User".to_string(),
                "assistant" => "Assistant".to_string(),
                "developer" => "Developer".to_string(),
                _ => "Message".to_string(),
            };
            event.summary = summarize_text(&body, 180);
            event.body = if body.is_empty() { stringify_map(payload) } else { body };
        }
        "user_message" => {
            let body = payload
                .get("message")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| stringify_map(payload));
            event.kind = "user_message".to_string();
            event.role = "user".to_string();
            event.title = "User".to_string();
            event.summary = summarize_text(&body, 180);
            event.body = body;
        }
        "agent_message" => {
            let body = payload
                .get("message")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| stringify_map(payload));
            event.kind = "assistant_message".to_string();
            event.role = "assistant".to_string();
            event.title = "Assistant".to_string();
            event.summary = summarize_text(&body, 180);
            event.body = body;
            event.status = payload
                .get("phase")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
        }
        "reasoning" => {
            let summary = extract_reasoning_summary(payload);
            event.kind = "reasoning".to_string();
            event.role = "assistant".to_string();
            event.title = "Reasoning".to_string();
            event.summary = summarize_text(&summary, 180);
            event.body = if summary.is_empty() {
                stringify_map(payload)
            } else {
                summary
            };
        }
        "function_call" => {
            let arguments = payload
                .get("arguments")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            event.kind = "function_call".to_string();
            event.role = "tool".to_string();
            event.title = payload
                .get("name")
                .and_then(JsonValue::as_str)
                .unwrap_or("Tool")
                .to_string();
            event.summary = summarize_text(&arguments, 180);
            event.body = arguments;
            event.call_id = payload
                .get("call_id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
        }
        "function_call_output" => {
            let output = payload
                .get("output")
                .map(stringify_json_or_string)
                .unwrap_or_default();
            event.kind = "function_call_output".to_string();
            event.role = "tool".to_string();
            event.title = "Tool Output".to_string();
            event.summary = summarize_text(&output, 180);
            event.body = output;
            event.call_id = payload
                .get("call_id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
        }
        "exec_command_end" => {
            let body = first_non_empty_owned(&[
                payload
                    .get("aggregated_output")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string),
                payload
                    .get("formatted_output")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string),
                join_stdout_stderr(payload),
            ]);
            event.kind = "exec_command".to_string();
            event.role = "tool".to_string();
            event.title = format_command_title(payload);
            event.summary = summarize_text(&body, 180);
            event.body = body;
            event.call_id = payload
                .get("call_id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            event.status = payload
                .get("status")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    payload
                        .get("exit_code")
                        .map(stringify_json_or_string)
                        .unwrap_or_default()
                });
        }
        "patch_apply_end" => {
            let stdout = payload.get("stdout").and_then(JsonValue::as_str).unwrap_or_default();
            let stderr = payload.get("stderr").and_then(JsonValue::as_str).unwrap_or_default();
            let changes = payload
                .get("changes")
                .map(stringify_json_or_string)
                .unwrap_or_default();
            let body = [stdout.trim(), stderr.trim(), changes.trim()]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            event.kind = "patch".to_string();
            event.role = "tool".to_string();
            event.title = "Patch Apply".to_string();
            event.summary = summarize_text(&body, 180);
            event.body = body;
            event.call_id = payload
                .get("call_id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            event.status = payload
                .get("status")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    payload
                        .get("success")
                        .map(stringify_json_or_string)
                        .unwrap_or_default()
                });
        }
        "web_search_call" | "web_search_end" => {
            let query = payload.get("query").and_then(JsonValue::as_str).unwrap_or_default();
            let action = payload
                .get("action")
                .map(stringify_json_or_string)
                .unwrap_or_default();
            event.kind = "web_search".to_string();
            event.role = "tool".to_string();
            event.title = "Web Search".to_string();
            event.summary = summarize_text(if !query.is_empty() { query } else { &action }, 180);
            event.body = if action.is_empty() { stringify_map(payload) } else { action };
            event.call_id = payload
                .get("call_id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            event.status = payload
                .get("status")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
        }
        "custom_tool_call" => {
            let body = payload.get("input").map(stringify_json_or_string).unwrap_or_default();
            event.kind = "custom_tool".to_string();
            event.role = "tool".to_string();
            event.title = payload
                .get("name")
                .and_then(JsonValue::as_str)
                .unwrap_or("Custom Tool")
                .to_string();
            event.summary = summarize_text(&body, 180);
            event.body = body;
            event.call_id = payload
                .get("call_id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            event.status = payload
                .get("status")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
        }
        "custom_tool_call_output" => {
            let body = payload
                .get("output")
                .map(stringify_json_or_string)
                .unwrap_or_default();
            event.kind = "custom_tool".to_string();
            event.role = "tool".to_string();
            event.title = "Custom Tool Output".to_string();
            event.summary = summarize_text(&body, 180);
            event.body = body;
            event.call_id = payload
                .get("call_id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
        }
        "view_image_tool_call" => {
            let body = payload.get("path").and_then(JsonValue::as_str).unwrap_or_default();
            event.kind = "image".to_string();
            event.role = "tool".to_string();
            event.title = "View Image".to_string();
            event.summary = summarize_text(body, 180);
            event.body = body.to_string();
            event.call_id = payload
                .get("call_id")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
        }
        "token_count" => {
            let summary = payload.get("info").map(stringify_json_or_string).unwrap_or_default();
            event.kind = "token_count".to_string();
            event.role = "system".to_string();
            event.title = "Token Count".to_string();
            event.summary = summarize_text(&summary, 180);
            event.body = stringify_map(payload);
        }
        "task_started" | "task_complete" | "item_completed" | "turn_aborted" | "thread_rolled_back"
        | "context_compacted" | "compacted" => {
            event.kind = "task".to_string();
            event.role = "system".to_string();
            event.title = payload_type.to_string();
            event.summary = summarize_text(&stringify_map(payload), 180);
            event.body = stringify_map(payload);
            event.status = payload
                .get("reason")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
        }
        "error" => {
            let message = payload
                .get("message")
                .and_then(JsonValue::as_str)
                .unwrap_or("Unknown error");
            event.kind = "error".to_string();
            event.role = "system".to_string();
            event.title = "Error".to_string();
            event.summary = summarize_text(message, 180);
            event.body = stringify_map(payload);
            event.status = "error".to_string();
        }
        _ => {}
    }

    event
}

fn get_session_backup_base_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Failed to resolve home directory.")?;
    Ok(home.join(".codex").join(SESSION_FAVORITE_ROOT_DIR))
}

fn get_session_favorite_dir(instance_id: &str, session_id: &str) -> Result<PathBuf, String> {
    Ok(get_session_backup_base_dir()?
        .join(sanitize_for_file_name(instance_id))
        .join(sanitize_for_file_name(session_id)))
}

fn is_session_favorited(instance_id: &str, session_id: &str) -> bool {
    get_session_favorite_dir(instance_id, session_id)
        .map(|path| path.exists())
        .unwrap_or(false)
}

fn copy_files_to_dir(
    target_dir: &Path,
    file_paths: &[PathBuf],
    manifest: Option<JsonValue>,
) -> Result<Vec<String>, String> {
    fs::create_dir_all(target_dir)
        .map_err(|error| format!("Failed to create backup dir {}: {}", target_dir.display(), error))?;

    let mut copied = Vec::new();
    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }
        let Some(file_name) = file_path.file_name() else {
            continue;
        };
        let target_path = target_dir.join(file_name);
        fs::copy(file_path, &target_path).map_err(|error| {
            format!(
                "Failed to backup {} to {}: {}",
                file_path.display(),
                target_path.display(),
                error
            )
        })?;
        copied.push(target_path.to_string_lossy().to_string());
    }

    if let Some(manifest_value) = manifest {
        let manifest_path = target_dir.join("manifest.json");
        let content = serde_json::to_string_pretty(&manifest_value)
            .map_err(|error| format!("Failed to serialize manifest for {}: {}", target_dir.display(), error))?;
        fs::write(&manifest_path, content)
            .map_err(|error| format!("Failed to write {}: {}", manifest_path.display(), error))?;
        copied.push(manifest_path.to_string_lossy().to_string());
    }

    Ok(copied)
}

fn build_favorite_manifest(item: &SessionMatch, file_paths: &[PathBuf]) -> Result<JsonValue, String> {
    Ok(json!({
        "session_id": item.snapshot.id,
        "instance_id": item.instance.id,
        "instance_name": item.instance.name,
        "favorited_at": Utc::now().to_rfc3339(),
        "cwd": item.snapshot.cwd,
        "model_provider": item.snapshot.model_provider,
        "source_paths": file_paths.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),
    }))
}

fn wal_path(db_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}-wal", db_path.to_string_lossy()))
}

fn shm_path(db_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}-shm", db_path.to_string_lossy()))
}

fn upsert_rollout_title(
    rollout_path: Option<&Path>,
    session_id: &str,
    title: &str,
) -> Result<bool, String> {
    let Some(path) = rollout_path else {
        return Ok(false);
    };
    if !path.exists() {
        return Ok(false);
    }

    let content = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read {}: {}", path.display(), error))?;
    let mut next_lines = Vec::new();
    let mut updated = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            next_lines.push(String::new());
            continue;
        }

        if !updated {
            match serde_json::from_str::<JsonValue>(trimmed) {
                Ok(mut parsed) => {
                    if update_session_meta_title(&mut parsed, session_id, title)? {
                        next_lines.push(
                            serde_json::to_string(&parsed)
                                .map_err(|error| format!("Failed to serialize session meta line: {}", error))?,
                        );
                        updated = true;
                        continue;
                    }
                    next_lines.push(trimmed.to_string());
                }
                Err(_) => next_lines.push(trimmed.to_string()),
            }
        } else {
            next_lines.push(trimmed.to_string());
        }
    }

    if !updated {
        return Ok(false);
    }

    let next_content = if next_lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", next_lines.join("\n"))
    };
    fs::write(path, next_content)
        .map_err(|error| format!("Failed to write {}: {}", path.display(), error))?;
    Ok(true)
}

fn update_session_meta_title(
    line: &mut JsonValue,
    session_id: &str,
    title: &str,
) -> Result<bool, String> {
    let Some(payload) = line.get_mut("payload").and_then(JsonValue::as_object_mut) else {
        return Ok(false);
    };
    let payload_type = payload
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    if payload_type != "session_meta" {
        return Ok(false);
    }

    if let Some(id) = payload.get("id").and_then(JsonValue::as_str) {
        if !id.eq_ignore_ascii_case(session_id) {
            return Ok(false);
        }
    }

    payload.insert("thread_name".to_string(), json!(title));
    payload.insert("title".to_string(), json!(title));
    Ok(true)
}

fn upsert_session_index(
    index_path: &Path,
    session_id: &str,
    title: &str,
    updated_at: &str,
    existing_entry: Option<&JsonValue>,
) -> Result<bool, String> {
    let content = if index_path.exists() {
        fs::read_to_string(index_path)
            .map_err(|error| format!("Failed to read {}: {}", index_path.display(), error))?
    } else {
        String::new()
    };

    let mut next_lines = Vec::new();
    let mut updated = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<JsonValue>(trimmed) {
            Ok(mut parsed) => {
                if parsed
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .map(|value| value.eq_ignore_ascii_case(session_id))
                    .unwrap_or(false)
                {
                    update_session_index_entry(&mut parsed, session_id, title, updated_at);
                    next_lines.push(
                        serde_json::to_string(&parsed)
                            .map_err(|error| format!("Failed to serialize session index entry: {}", error))?,
                    );
                    updated = true;
                } else {
                    next_lines.push(trimmed.to_string());
                }
            }
            Err(_) => next_lines.push(trimmed.to_string()),
        }
    }

    if !updated {
        let mut entry = existing_entry.cloned().unwrap_or_else(|| json!({}));
        update_session_index_entry(&mut entry, session_id, title, updated_at);
        next_lines.push(
            serde_json::to_string(&entry)
                .map_err(|error| format!("Failed to serialize new session index entry: {}", error))?,
        );
    }

    let next_content = if next_lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", next_lines.join("\n"))
    };
    fs::write(index_path, next_content)
        .map_err(|error| format!("Failed to write {}: {}", index_path.display(), error))?;
    Ok(true)
}

fn update_session_index_entry(entry: &mut JsonValue, session_id: &str, title: &str, updated_at: &str) {
    if !entry.is_object() {
        *entry = json!({});
    }
    if let Some(object) = entry.as_object_mut() {
        object.insert("id".to_string(), json!(session_id));
        object.insert("thread_name".to_string(), json!(title));
        object.insert("updated_at".to_string(), json!(updated_at));
    }
}

fn upsert_thread_title(
    root_dir: &Path,
    snapshot: &SessionSnapshot,
    title: &str,
    updated_at: &str,
) -> Result<bool, String> {
    let db_path = root_dir.join(STATE_DB_FILE);
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create {}: {}", parent.display(), error))?;
    }

    let connection = Connection::open(&db_path)
        .map_err(|error| format!("Failed to open {}: {}", db_path.display(), error))?;
    connection
        .busy_timeout(Duration::from_secs(3))
        .map_err(|error| format!("Failed to set busy timeout for {}: {}", db_path.display(), error))?;
    ensure_threads_table(&connection)?;

    let existing_created_at = connection
        .query_row(
            "SELECT created_at FROM threads WHERE id = ?1 LIMIT 1",
            [&snapshot.id],
            |row| row.get::<usize, i64>(0),
        )
        .ok();
    let created_at = existing_created_at
        .or(snapshot.created_at)
        .unwrap_or_else(|| parse_rfc3339_to_timestamp(updated_at).unwrap_or_else(now_timestamp));
    let updated_at_seconds = parse_rfc3339_to_timestamp(updated_at).unwrap_or_else(now_timestamp);
    let rollout_path = snapshot
        .rollout_path
        .as_ref()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let model_provider = if snapshot.model_provider.trim().is_empty() {
        "openai".to_string()
    } else {
        snapshot.model_provider.clone()
    };

    connection
        .execute(
            r#"
            INSERT INTO threads (
              id,
              rollout_path,
              created_at,
              updated_at,
              source,
              model_provider,
              cwd,
              title,
              sandbox_policy,
              approval_mode,
              tokens_used,
              has_user_event,
              archived,
              cli_version,
              first_user_message,
              memory_mode
            ) VALUES (
              ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, 1, 0, ?11, ?12, 'enabled'
            )
            ON CONFLICT(id) DO UPDATE SET
              title = excluded.title,
              updated_at = excluded.updated_at,
              cwd = excluded.cwd,
              rollout_path = excluded.rollout_path,
              model_provider = excluded.model_provider,
              first_user_message = excluded.first_user_message
            "#,
            [
                &snapshot.id as &dyn rusqlite::ToSql,
                &rollout_path,
                &created_at,
                &updated_at_seconds,
                &"cli",
                &model_provider,
                &snapshot.cwd,
                &title,
                &"{\"type\":\"danger-full-access\"}",
                &"never",
                &"cockpit-tools",
                &title,
            ],
        )
        .map_err(|error| format!("Failed to update {} in {}: {}", snapshot.id, db_path.display(), error))?;

    Ok(true)
}

fn ensure_threads_table(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS threads (
              id TEXT PRIMARY KEY,
              rollout_path TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL,
              source TEXT NOT NULL,
              model_provider TEXT NOT NULL,
              cwd TEXT NOT NULL,
              title TEXT NOT NULL,
              sandbox_policy TEXT NOT NULL,
              approval_mode TEXT NOT NULL,
              tokens_used INTEGER NOT NULL DEFAULT 0,
              has_user_event INTEGER NOT NULL DEFAULT 0,
              archived INTEGER NOT NULL DEFAULT 0,
              archived_at INTEGER,
              git_sha TEXT,
              git_branch TEXT,
              git_origin_url TEXT,
              cli_version TEXT NOT NULL DEFAULT '',
              first_user_message TEXT NOT NULL DEFAULT '',
              agent_nickname TEXT,
              agent_role TEXT,
              memory_mode TEXT NOT NULL DEFAULT 'enabled',
              model TEXT,
              reasoning_effort TEXT,
              agent_path TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_threads_updated_at ON threads(updated_at DESC, id DESC);
            "#,
        )
        .map_err(|error| format!("Failed to ensure threads table: {}", error))
}

fn open_readonly_connection(db_path: &Path) -> Result<Connection, String> {
    let mut uri = Url::from_file_path(db_path)
        .map_err(|_| format!("Failed to build readonly URI for {}", db_path.display()))?;
    uri.set_query(Some("mode=ro"));
    Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| format!("Failed to open {} readonly: {}", db_path.display(), error))
}

fn read_thread_columns(connection: &Connection) -> Result<Vec<String>, String> {
    let mut statement = connection
        .prepare("PRAGMA table_info(threads)")
        .map_err(|error| format!("Failed to inspect threads table: {}", error))?;
    let mut rows = statement
        .query([])
        .map_err(|error| format!("Failed to read threads schema: {}", error))?;
    let mut columns = Vec::new();

    while let Some(row) = rows
        .next()
        .map_err(|error| format!("Failed to parse threads schema: {}", error))?
    {
        columns.push(
            row.get::<usize, String>(1)
                .map_err(|error| format!("Failed to parse threads column: {}", error))?,
        );
    }

    if columns.is_empty() {
        return Err("Threads table is missing.".to_string());
    }

    Ok(columns)
}

impl ThreadRowData {
    fn get_value(&self, column: &str) -> Option<&Value> {
        self.columns
            .iter()
            .position(|item| item == column)
            .and_then(|index| self.values.get(index))
    }

    fn get_text(&self, column: &str) -> Option<String> {
        match self.get_value(column)? {
            Value::Text(value) => Some(value.clone()),
            Value::Integer(value) => Some(value.to_string()),
            Value::Real(value) => Some(value.to_string()),
            _ => None,
        }
    }

    fn get_i64(&self, column: &str) -> Option<i64> {
        match self.get_value(column)? {
            Value::Integer(value) => Some(*value),
            Value::Text(value) => value.parse::<i64>().ok(),
            _ => None,
        }
    }
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn first_non_empty<'a>(values: &[Option<&'a str>]) -> Option<&'a str> {
    values
        .iter()
        .flatten()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
}

fn first_non_empty_owned(values: &[Option<String>]) -> String {
    values
        .iter()
        .flatten()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn normalize_title(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 120 {
        collapsed.chars().take(120).collect::<String>() + "..."
    } else {
        collapsed
    }
}

fn extract_message_content(payload: &Map<String, JsonValue>) -> String {
    let Some(content) = payload.get("content").and_then(JsonValue::as_array) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for item in content {
        let Some(object) = item.as_object() else {
            continue;
        };
        let item_type = object
            .get("type")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if matches!(item_type, "input_text" | "output_text" | "text") {
            if let Some(text) = object.get("text").and_then(JsonValue::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
            }
        }
    }
    parts.join("\n\n")
}

fn extract_reasoning_summary(payload: &Map<String, JsonValue>) -> String {
    if let Some(content) = payload.get("content").and_then(JsonValue::as_str) {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(summary) = payload.get("summary") {
        if let Some(items) = summary.as_array() {
            let text = items
                .iter()
                .map(stringify_json_or_string)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return text;
            }
        }
        let text = stringify_json_or_string(summary);
        if !text.trim().is_empty() {
            return text;
        }
    }
    String::new()
}

fn format_command_title(payload: &Map<String, JsonValue>) -> String {
    let Some(command) = payload.get("command") else {
        return "Command".to_string();
    };
    if let Some(items) = command.as_array() {
        let parts = items
            .iter()
            .filter_map(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return summarize_text(&parts.join(" "), 120);
        }
    }
    summarize_text(&stringify_json_or_string(command), 120)
}

fn join_stdout_stderr(payload: &Map<String, JsonValue>) -> Option<String> {
    let stdout = payload
        .get("stdout")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let stderr = payload
        .get("stderr")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let joined = [stdout, stderr]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn summarize_text(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return String::new();
    }
    if normalized.chars().count() <= limit {
        return normalized;
    }
    normalized.chars().take(limit).collect::<String>() + "..."
}

fn stringify_map(value: &Map<String, JsonValue>) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| JsonValue::Object(value.clone()).to_string())
}

fn stringify_json_or_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => String::new(),
        JsonValue::String(text) => text.clone(),
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn normalize_timestamp_string(value: &str) -> String {
    parse_rfc3339_to_datetime(Some(value))
        .map(|value| value.to_rfc3339())
        .unwrap_or_else(|| value.to_string())
}

fn parse_rfc3339_to_datetime(value: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn parse_rfc3339_to_timestamp(value: &str) -> Option<i64> {
    parse_rfc3339_to_datetime(Some(value)).map(|value| value.timestamp())
}

fn system_time_to_timestamp(value: Option<SystemTime>) -> Option<i64> {
    let time = value?;
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    Some(duration.as_secs() as i64)
}

fn now_timestamp() -> i64 {
    Utc::now().timestamp()
}

fn sanitize_for_file_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
}
