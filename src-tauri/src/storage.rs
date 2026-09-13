use crate::{AppConfig, AppError};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

pub fn load_config(app: &AppHandle) -> Result<AppConfig, AppError> {
    let path = config_path(app)?;
    if !path.exists() {
        let config = AppConfig::default();
        save_config(app, &config)?;
        return Ok(config);
    }

    let content = fs::read_to_string(&path).map_err(|error| {
        AppError::with_detail(
            "config_read_failed",
            "配置读取失败，请检查 AutoFlow 的本地数据目录权限",
            error.to_string(),
        )
    })?;

    match serde_json::from_str::<AppConfig>(&content) {
        Ok(config) => {
            let (config, migrated) = config.migrate()?;
            config.validate()?;
            if migrated {
                save_config(app, &config)?;
            }
            Ok(config)
        }
        Err(error) => {
            let backup_path = path.with_extension("json.corrupt");
            let _ = fs::copy(&path, backup_path);
            let config = AppConfig::default();
            save_config(app, &config)?;
            log::warn!("配置损坏，已切换到安全默认配置: {error}");
            Ok(config)
        }
    }
}

pub fn save_config(app: &AppHandle, config: &AppConfig) -> Result<(), AppError> {
    config.validate()?;
    let path = config_path(app)?;
    let directory = path
        .parent()
        .ok_or_else(|| AppError::internal("配置目录解析失败，请重启 AutoFlow"))?;
    fs::create_dir_all(directory).map_err(|error| {
        AppError::with_detail(
            "config_directory_failed",
            "无法创建 AutoFlow 配置目录，请检查本地权限",
            error.to_string(),
        )
    })?;

    let content = serde_json::to_string_pretty(config).map_err(|error| {
        AppError::with_detail("config_encode_failed", "配置序列化失败", error.to_string())
    })?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, content).map_err(|error| {
        AppError::with_detail(
            "config_write_failed",
            "配置写入失败，请检查磁盘空间和权限",
            error.to_string(),
        )
    })?;

    if path.exists() {
        let backup_path = path.with_extension("json.bak");
        let _ = fs::copy(&path, backup_path);
        fs::remove_file(&path).map_err(|error| {
            AppError::with_detail(
                "config_replace_failed",
                "旧配置替换失败，请重试保存",
                error.to_string(),
            )
        })?;
    }
    fs::rename(&temp_path, &path).map_err(|error| {
        AppError::with_detail(
            "config_rename_failed",
            "配置保存失败，请重试保存",
            error.to_string(),
        )
    })?;
    Ok(())
}

fn config_path(app: &AppHandle) -> Result<PathBuf, AppError> {
    let directory = app.path().app_config_dir().map_err(|error| {
        AppError::with_detail(
            "config_path_failed",
            "无法定位 AutoFlow 本地配置目录",
            error.to_string(),
        )
    })?;
    Ok(directory.join("config.json"))
}
