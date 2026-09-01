use crate::AppError;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "AutoFlow";

#[cfg(windows)]
pub fn apply(enabled: bool) -> Result<(), AppError> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
    };

    let key = wide(RUN_KEY);
    let value_name = wide(VALUE_NAME);
    let status = if enabled {
        let executable = std::env::current_exe().map_err(|error| {
            AppError::with_detail(
                "autostart_path_failed",
                "无法读取 AutoFlow 的启动路径",
                error.to_string(),
            )
        })?;
        let command = wide(&format!("\"{}\"", executable.display()));
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(value_name.as_ptr()),
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * std::mem::size_of::<u16>()) as u32,
            )
        }
    } else {
        unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(value_name.as_ptr()),
            )
        }
    };

    // Deleting a value that has never been created is already the desired state.
    if status.0 == 0 || (!enabled && status.0 == 2) {
        return Ok(());
    }
    Err(AppError::with_detail(
        "autostart_registry_failed",
        if enabled {
            "无法设置开机自启动"
        } else {
            "无法取消开机自启动"
        },
        format!("Windows 注册表错误代码：{}", status.0),
    ))
}

#[cfg(not(windows))]
pub fn apply(_enabled: bool) -> Result<(), AppError> {
    Err(AppError::invalid(
        "autostart_unsupported",
        "开机自启动目前只支持 Windows 桌面端",
    ))
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
