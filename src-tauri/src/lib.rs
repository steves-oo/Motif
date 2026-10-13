use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

mod autostart;

// ── 数据目录：文件即真相，指针存本机 app-config，不参与同步 ──────────────

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).ok();
    Ok(dir.join("config.json"))
}

/// 未配置时的默认数据目录（沿用旧的选路逻辑，换成放文件的目录）
fn default_data_dir(app: &AppHandle) -> PathBuf {
    if let Ok(dir) = std::env::var("MOTIF_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "windows") && std::path::Path::new("D:\\").exists() {
        return PathBuf::from("D:/data/Motif");
    }
    app.path()
        .app_data_dir()
        .map(|d| d.join("data"))
        .unwrap_or_else(|_| PathBuf::from("motif-data"))
}

fn ensure_subdirs(dir: &PathBuf) {
    fs::create_dir_all(dir.join("items")).ok();
    fs::create_dir_all(dir.join("tags")).ok();
}

/// 读整个 config.json;不存在或损坏时返回空对象
fn read_config(app: &AppHandle) -> serde_json::Map<String, serde_json::Value> {
    config_path(app)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| match v {
            serde_json::Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

/// 读-改-写合并单个键,不冲掉其他键(data_dir / shortcut 并存)
fn write_config_key(app: &AppHandle, key: &str, value: serde_json::Value) -> Result<(), String> {
    let cfg = config_path(app)?;
    let mut map = read_config(app);
    map.insert(key.to_string(), value);
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(map))
        .map_err(|e| e.to_string())?;
    fs::write(&cfg, text).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_data_dir(app: AppHandle) -> Result<String, String> {
    let configured = read_config(&app)
        .get("data_dir")
        .and_then(|x| x.as_str())
        .map(PathBuf::from);
    let dir = configured.unwrap_or_else(|| default_data_dir(&app));
    ensure_subdirs(&dir);
    Ok(dir.to_string_lossy().to_string())
}

#[tauri::command]
fn set_data_dir(app: AppHandle, path: String) -> Result<(), String> {
    write_config_key(&app, "data_dir", serde_json::Value::String(path.clone()))?;
    ensure_subdirs(&PathBuf::from(&path));
    Ok(())
}

// ── 全局捕获快捷键:存本机 config.json 的 "shortcut",不参与同步 ─────────

const DEFAULT_SHORTCUT: &str = "CmdOrCtrl+Shift+Space";

/// 当前生效(或应生效)的 accelerator 字符串;各命令持锁执行,注册操作因此串行
struct CurrentShortcut(Mutex<String>);

/// 启动时读取:缺省或无法解析时用默认值
fn load_shortcut(app: &AppHandle) -> String {
    read_config(app)
        .get("shortcut")
        .and_then(|x| x.as_str())
        .filter(|s| s.parse::<Shortcut>().is_ok())
        .map(String::from)
        .unwrap_or_else(|| DEFAULT_SHORTCUT.to_string())
}

/// 托盘"注册失败"提示项的显示/隐藏
fn set_tray_shortcut_err(app: &AppHandle, show: bool) {
    let Some(state) = app.try_state::<Mutex<TrayItems>>() else {
        return;
    };
    // 先改标记并克隆句柄再放锁:菜单操作会派发到主线程等待,持锁等待可能与主线程上的 set_tray_language 互锁
    let (menu, item) = {
        let Ok(mut t) = state.lock() else {
            return;
        };
        if t.err_shown == show {
            return;
        }
        t.err_shown = show;
        (t.menu.clone(), t.shortcut_err.clone())
    };
    let _ = if show {
        // 插在"快速捕获"之后,与启动时的位置一致
        menu.insert(&item, 2)
    } else {
        menu.remove(&item)
    };
}

// 以下命令一律 async(不在主线程执行):插件注册要派发到主线程并阻塞等待结果,
// 若主线程上的同步命令去抢 CurrentShortcut 锁会互锁

#[tauri::command]
async fn get_shortcut(app: AppHandle) -> Result<String, String> {
    let state = app.state::<CurrentShortcut>();
    let current = state.0.lock().map_err(|e| e.to_string())?;
    Ok(current.clone())
}

#[tauri::command]
async fn set_shortcut(app: AppHandle, accel: String) -> Result<(), String> {
    let state = app.state::<CurrentShortcut>();
    let mut current = state.0.lock().map_err(|e| e.to_string())?;
    let gs = app.global_shortcut();
    let new_sc: Shortcut = accel.parse().map_err(|e| format!("{e}"))?;
    let old_sc: Option<Shortcut> = current.parse().ok();

    // 1. 先注销当前(录制期间已被暂停时无需注销)
    let old_registered = old_sc.filter(|s| gs.is_registered(*s));
    if let Some(old) = old_registered {
        gs.unregister(old).map_err(|e| e.to_string())?;
    }

    // 2. 注册新的;失败则重新注册旧的并返回错误,不写 config
    if let Err(e) = gs.register(new_sc) {
        if let Some(old) = old_registered {
            if gs.register(old).is_err() {
                set_tray_shortcut_err(&app, true);
            }
        }
        return Err(e.to_string());
    }

    // 3. 成功:记为当前,移除托盘失败提示,写 config
    *current = accel.clone();
    set_tray_shortcut_err(&app, false);
    write_config_key(&app, "shortcut", serde_json::Value::String(accel))?;
    Ok(())
}

/// 录制组合键期间暂停当前快捷键,避免按下旧组合时呼出捕获窗抢走焦点
#[tauri::command]
async fn pause_shortcut(app: AppHandle) -> Result<(), String> {
    let state = app.state::<CurrentShortcut>();
    let current = state.0.lock().map_err(|e| e.to_string())?;
    let gs = app.global_shortcut();
    if let Ok(sc) = current.parse::<Shortcut>() {
        if gs.is_registered(sc) {
            gs.unregister(sc).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 录制结束后恢复:确保当前快捷键处于注册状态(幂等)
#[tauri::command]
async fn resume_shortcut(app: AppHandle) -> Result<(), String> {
    let state = app.state::<CurrentShortcut>();
    let current = state.0.lock().map_err(|e| e.to_string())?;
    let gs = app.global_shortcut();
    let sc: Shortcut = current.parse().map_err(|e| format!("{e}"))?;
    if gs.is_registered(sc) {
        return Ok(());
    }
    if let Err(e) = gs.register(sc) {
        set_tray_shortcut_err(&app, true);
        return Err(e.to_string());
    }
    set_tray_shortcut_err(&app, false);
    Ok(())
}

// ── 记录文件读写（无新 crate，纯 std::fs） ─────────────────────────────

#[derive(serde::Serialize)]
struct Record {
    name: String,
    content: String,
}

/// 读某子目录（items/tags）下所有 .md 的文件名 + 内容
#[tauri::command]
fn list_records(dir: String, sub: String) -> Result<Vec<Record>, String> {
    let path = PathBuf::from(&dir).join(&sub);
    fs::create_dir_all(&path).ok();
    let mut out = Vec::new();
    for entry in fs::read_dir(&path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(".tmp-") {
            continue; // 跳过原子写入的临时文件
        }
        if let Ok(content) = fs::read_to_string(&p) {
            out.push(Record { name, content });
        }
    }
    Ok(out)
}

/// 原子写：先写同目录临时文件，再 rename 落盘
#[tauri::command]
fn write_record_atomic(dir: String, sub: String, name: String, content: String) -> Result<(), String> {
    let subdir = PathBuf::from(&dir).join(&sub);
    fs::create_dir_all(&subdir).map_err(|e| e.to_string())?;
    let tmp = subdir.join(format!(".tmp-{}", name));
    let final_path = subdir.join(&name);
    fs::write(&tmp, content.as_bytes()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &final_path).map_err(|e| e.to_string())?;
    Ok(())
}

/// 把冲突落败文件归档到 conflicts/（同名则追加序号，绝不覆盖）
#[tauri::command]
fn move_to_conflicts(dir: String, sub: String, name: String) -> Result<(), String> {
    let base = PathBuf::from(&dir);
    let from = base.join(&sub).join(&name);
    if !from.exists() {
        return Ok(()); // 另一个窗口已处理过
    }
    let conflicts = base.join("conflicts");
    fs::create_dir_all(&conflicts).map_err(|e| e.to_string())?;
    let mut target = conflicts.join(&name);
    if target.exists() {
        let np = std::path::Path::new(&name);
        let stem = np.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
        let ext = np.extension().and_then(|s| s.to_str()).unwrap_or("md");
        let mut i = 2;
        loop {
            let cand = conflicts.join(format!("{}-{}.{}", stem, i, ext));
            if !cand.exists() {
                target = cand;
                break;
            }
            i += 1;
        }
    }
    fs::rename(&from, &target).map_err(|e| e.to_string())?;
    Ok(())
}

/// 把记录从一个子目录原样挪到另一个（字节不变）。源文件不存在视为已挪过。
#[tauri::command]
fn move_record(dir: String, from_sub: String, to_sub: String, name: String) -> Result<(), String> {
    let base = PathBuf::from(&dir);
    let from = base.join(&from_sub).join(&name);
    if !from.exists() {
        return Ok(());
    }
    let to_dir = base.join(&to_sub);
    fs::create_dir_all(&to_dir).map_err(|e| e.to_string())?;
    match fs::rename(&from, to_dir.join(&name)) {
        Err(e) if from.exists() => Err(e.to_string()),
        _ => Ok(()), // 成功,或期间被另一个窗口挪走
    }
}

/// 删掉某个位置上的记录文件:仅用于记录已写到新位置后清理旧位置,不是软删除。
#[tauri::command]
fn remove_record(dir: String, sub: String, name: String) -> Result<(), String> {
    match fs::remove_file(PathBuf::from(&dir).join(&sub).join(&name)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

#[tauri::command]
fn hide_capture(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("capture") {
        let _ = win.hide();
    }
}

#[derive(serde::Deserialize)]
struct MdFile {
    name: String,
    content: String,
}

#[tauri::command]
fn write_md_files(dir: String, files: Vec<MdFile>) -> Result<usize, String> {
    let base = std::path::PathBuf::from(&dir);
    for file in &files {
        let path = base.join(&file.name);
        std::fs::write(&path, &file.content).map_err(|e| e.to_string())?;
    }
    Ok(files.len())
}

/// 显示并前置主窗口(托盘菜单、托盘左键、macOS Dock 重开共用)
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

struct TrayItems {
    menu: Menu<tauri::Wry>,
    open_main: MenuItem<tauri::Wry>,
    quick_capture: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
    // 始终持有;仅在注册失败时插入菜单,err_shown 标记是否在菜单中
    shortcut_err: MenuItem<tauri::Wry>,
    err_shown: bool,
}

#[tauri::command]
fn set_tray_language(
    state: tauri::State<'_, Mutex<TrayItems>>,
    labels: Vec<(String, String)>,
) -> Result<(), String> {
    let handles = state.lock().map_err(|e| e.to_string())?;
    for (id, text) in &labels {
        match id.as_str() {
            "open_main" => handles.open_main.set_text(text).map_err(|e| e.to_string())?,
            "quick_capture" => handles
                .quick_capture
                .set_text(text)
                .map_err(|e| e.to_string())?,
            "quit" => handles.quit.set_text(text).map_err(|e| e.to_string())?,
            "shortcut_err" => handles
                .shortcut_err
                .set_text(text)
                .map_err(|e| e.to_string())?,
            _ => {}
        }
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // 开机自启动:默认关,由设置面板开关;自启动项带 --autostart 参数
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        if let Some(win) = app.get_webview_window("capture") {
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            // 本机配置的快捷键;缺省/非法 → CmdOrCtrl+Shift+Space(CmdOrCtrl:Windows 为 Ctrl,macOS 为 Cmd)
            let accel = load_shortcut(app.handle());
            let shortcut_result = app.global_shortcut().register(accel.as_str());
            if let Err(ref e) = shortcut_result {
                eprintln!("{accel} 注册失败: {e}");
            }
            app.manage(CurrentShortcut(Mutex::new(accel)));

            let main_win = app.get_webview_window("main").unwrap();
            let mw = main_win.clone();
            main_win.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = mw.hide();
                }
            });
            // 主窗口配置为初始隐藏:开机自启动只进托盘,手动启动才显示
            if !std::env::args().any(|a| a == "--autostart") {
                let _ = main_win.show();
                let _ = main_win.set_focus();
            }

            let cap_win = app.get_webview_window("capture").unwrap();
            let cw = cap_win.clone();
            cap_win.on_window_event(move |event| {
                if let WindowEvent::Focused(false) = event {
                    let _ = cw.hide();
                }
            });

            let open_main =
                MenuItem::with_id(app, "open_main", "打开主窗口", true, None::<&str>)?;
            let quick_capture =
                MenuItem::with_id(app, "quick_capture", "快速捕获", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            // JS 侧启动后会用 set_tray_language 覆盖为当前语言文案
            let shortcut_err = MenuItem::with_id(
                app,
                "shortcut_err",
                "⚠ 全局快捷键注册失败,请在设置中更换",
                false,
                None::<&str>,
            )?;

            let menu = if shortcut_result.is_err() {
                Menu::with_items(app, &[&open_main, &quick_capture, &shortcut_err, &quit])?
            } else {
                Menu::with_items(app, &[&open_main, &quick_capture, &quit])?
            };

            // 保存句柄:语言切换改文案 + 注册失败提示项的增删
            app.manage(Mutex::new(TrayItems {
                menu: menu.clone(),
                open_main: open_main.clone(),
                quick_capture: quick_capture.clone(),
                quit: quit.clone(),
                shortcut_err,
                err_shown: shortcut_result.is_err(),
            }));

            let tray = TrayIconBuilder::new()
                .tooltip("Motif")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open_main" => show_main(app),
                    "quick_capture" => {
                        if let Some(w) = app.get_webview_window("capture") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                });

            // macOS:单色模板图标(深浅色菜单栏均清晰);Windows/其他:沿用彩色图标
            #[cfg(target_os = "macos")]
            let tray = tray
                .icon(tauri::image::Image::from_bytes(include_bytes!(
                    "../icons/tray-template.png"
                ))?)
                .icon_as_template(true);
            #[cfg(not(target_os = "macos"))]
            let tray = tray.icon(app.default_window_icon().expect("app icon missing").clone());

            tray.build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            hide_capture,
            write_md_files,
            set_tray_language,
            get_data_dir,
            set_data_dir,
            get_shortcut,
            set_shortcut,
            pause_shortcut,
            resume_shortcut,
            list_records,
            write_record_atomic,
            move_to_conflicts,
            move_record,
            remove_record,
            autostart::set_autostart,
            autostart::is_autostart_enabled
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| match event {
            // macOS:关闭按钮只是隐藏,点 Dock 图标会发 Reopen,需在此把主窗口找回来
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => show_main(_app),
            _ => {}
        });
}
