use std::path::{Path, PathBuf};

pub fn vscode_data_root_candidates() -> Result<Vec<PathBuf>, String> {
    #[cfg(target_os = "windows")]
    {
        let appdata =
            std::env::var("APPDATA").map_err(|_| "无法获取 APPDATA 环境变量".to_string())?;
        let appdata = PathBuf::from(appdata);
        return Ok(vec![appdata.join("Code"), appdata.join("Code - Insiders")]);
    }

    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or("无法获取用户主目录")?;
        let base = home.join("Library").join("Application Support");
        return Ok(vec![base.join("Code"), base.join("Code - Insiders")]);
    }

    #[cfg(target_os = "linux")]
    {
        let base = if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
            let trimmed = xdg_config_home.trim();
            if trimmed.is_empty() {
                dirs::home_dir()
                    .ok_or("无法获取用户主目录".to_string())?
                    .join(".config")
            } else {
                PathBuf::from(trimmed)
            }
        } else {
            dirs::home_dir()
                .ok_or("无法获取用户主目录".to_string())?
                .join(".config")
        };
        return Ok(vec![base.join("Code"), base.join("Code - Insiders")]);
    }

    #[allow(unreachable_code)]
    Err("GitHub Copilot 仅支持 macOS、Windows 和 Linux".to_string())
}

pub fn resolve_preferred_vscode_data_root() -> Result<PathBuf, String> {
    let candidates = vscode_data_root_candidates()?;
    for root in &candidates {
        if root.exists() {
            return Ok(root.clone());
        }
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| "未找到 VS Code 候选目录".to_string())
}

pub fn resolve_vscode_data_root_for_state_db() -> Result<PathBuf, String> {
    let candidates = vscode_data_root_candidates()?;
    for root in &candidates {
        if vscode_state_db_path(root).exists() {
            return Ok(root.clone());
        }
    }
    for root in &candidates {
        if root.exists() {
            return Ok(root.clone());
        }
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| "未找到 VS Code 候选目录".to_string())
}

pub fn resolve_vscode_data_root(user_data_dir: Option<&str>) -> Result<PathBuf, String> {
    if let Some(raw) = user_data_dir {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    resolve_vscode_data_root_for_state_db()
}

pub fn vscode_state_db_path(data_root: &Path) -> PathBuf {
    data_root
        .join("User")
        .join("globalStorage")
        .join("state.vscdb")
}

#[cfg(target_os = "windows")]
pub fn vscode_local_state_path(data_root: &Path) -> PathBuf {
    data_root.join("Local State")
}
