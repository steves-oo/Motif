//! 开机自启动命令（替代前端直接调插件）。
//!
//! 背景：上游 tauri-plugin-autostart 依赖的 auto-launch 0.5.0 在 Windows 上写注册表
//! `HKCU\...\Run` 时不对 exe 路径加引号。安装路径一旦含空格（默认的
//! `C:\Program Files\Motif\Motif.exe` 就有），登录时系统会把 `C:\Program`
//! 当成可执行文件，静默失败 —— 设置页看着是开了，开机就是不起。
//!
//! 因此 Windows 走这里的自实现：值写成 `"C:\...\Motif.exe" --autostart`。
//! 注册表值名沿用插件用的 `package_info().name`（即 Cargo 包名 "motif"），
//! 老版本写坏的条目会被直接覆盖，老用户升级后自动修复。
//! macOS / Linux 继续委托给插件（那边工作正常）。

use tauri::AppHandle;

#[cfg(target_os = "windows")]
mod imp {
    use super::AppHandle;
    use std::env::current_exe;
    use winreg::enums::{RegType, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::{RegKey, RegValue};

    const RUN_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED_KEY: &str =
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    /// 与上游 auto-launch 一致的"已启用"标记（任务管理器 → 启动 页用）
    const APPROVED_ENABLED: [u8; 12] = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    /// 与 lib.rs 里传给插件 init 的 args 保持一致
    const ARGS: &str = "--autostart";

    fn entry_name(app: &AppHandle) -> String {
        app.package_info().name.clone()
    }

    /// 期望的注册表值：路径必须加引号，否则含空格路径开机静默失败
    fn expected_value() -> Result<String, String> {
        let exe = current_exe().map_err(|e| e.to_string())?;
        Ok(format!("\"{}\" {}", exe.display(), ARGS))
    }

    fn open_run_key(write: bool) -> Result<RegKey, String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, if write { KEY_SET_VALUE } else { KEY_READ })
            .map_err(|e| e.to_string())
    }

    fn write_entry(name: &str, value: &str) -> Result<(), String> {
        // set_value 的泛型按上游 auto-launch 的传法：name 用 &str、value 用 &String
        let value = value.to_string();
        open_run_key(true)?
            .set_value(name, &value)
            .map_err(|e| e.to_string())?;
        // 让任务管理器"启动"页显示为已启用（与上游行为一致）
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(APPROVED_KEY, KEY_SET_VALUE)
        {
            let _ = k.set_raw_value(
                name,
                &RegValue {
                    vtype: RegType::REG_BINARY,
                    bytes: APPROVED_ENABLED.to_vec(),
                },
            );
        }
        Ok(())
    }

    pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), String> {
        let name = entry_name(app);
        if enabled {
            write_entry(&name, &expected_value()?)?;
        } else if open_run_key(true)?.get_value::<String, _>(&name).is_ok() {
            open_run_key(true)?
                .delete_value(&name)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn is_enabled(app: &AppHandle) -> Result<bool, String> {
        let name = entry_name(app);
        let current: String = match open_run_key(false)?.get_value(&name) {
            Ok(v) => v,
            Err(_) => return Ok(false),
        };
        // 老版本写坏的条目（路径没加引号）：读到就地修复，用户不用手动开关一次
        if let Ok(expected) = expected_value() {
            if current != expected {
                let _ = write_entry(&name, &expected);
            }
        }
        // 用户在任务管理器里手动禁用过 → 视为关闭（与上游判定一致）
        let approved = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(APPROVED_KEY, KEY_READ)
            .ok()
            .and_then(|k| k.get_raw_value(&name).ok())
            .map(|v| v.bytes.len() >= 8 && v.bytes.iter().rev().take(8).all(|b| *b == 0));
        Ok(approved.unwrap_or(true))
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::AppHandle;
    use tauri_plugin_autostart::ManagerExt;

    pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), String> {
        let manager = app.autolaunch();
        let result = if enabled {
            manager.enable()
        } else {
            manager.disable()
        };
        result.map_err(|e| e.to_string())
    }

    pub fn is_enabled(app: &AppHandle) -> Result<bool, String> {
        app.autolaunch().is_enabled().map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    imp::set_enabled(&app, enabled)
}

#[tauri::command]
pub fn is_autostart_enabled(app: AppHandle) -> Result<bool, String> {
    imp::is_enabled(&app)
}
