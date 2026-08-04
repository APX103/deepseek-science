// Deepseek Science Tauri 壳：拉起后端二进制 + 注入端口 + 加载前端。
//
// 职责（modules.md §12 / tech-stack Tauri 栈）：
// 1. 找空闲端口（默认 17896）
// 2. spawn 后端二进制 dss-backend serve --port <port>
// 3. 把端口注入 webview（localStorage dss_backend_port + window.__BACKEND_PORT__）
// 4. 关窗时杀后端进程组，并把后端 stdout/stderr 落到日志文件

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::{Command, Stdio};
use tauri::Manager;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const DEFAULT_PORT: u16 = 17896;
const BACKEND_LOG_FILE: &str = "logs/backend.log";

/// 找一个可用端口：优先 DEFAULT_PORT，被占则递增。
fn find_free_port(start: u16) -> u16 {
    for port in start..start + 100 {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    start
}

/// 后端数据目录：与后端约定一致 `~/.deepseek-science`。
fn data_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    app.path()
        .home_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".deepseek-science")
}

/// 解析后端二进制路径：
/// - 开发：项目根 target/debug/dss-backend（或 release，见 `preferred_debug`）
/// - 打包：Tauri 资源目录里的 dss-backend
fn backend_binary_path(app: &tauri::AppHandle) -> std::path::PathBuf {
    // 打包模式：sidecar / resource。
    if let Ok(dir) = app.path().resource_dir() {
        let candidate = dir.join("dss-backend");
        if candidate.exists() {
            return candidate;
        }
    }
    // 开发模式：workspace target/debug。
    // 从 src-tauri 向上两级 = workspace root。
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let workspace_root = manifest_dir
        .parent() // src-tauri 的上级 = workspace root
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| manifest_dir.clone());
    // 优先 release（如果存在），否则 debug。
    let release = workspace_root.join("target").join("release").join("dss-backend");
    if release.exists() {
        return release;
    }
    workspace_root.join("target").join("debug").join("dss-backend")
}

/// 打开后端日志文件，返回可写的 File。
fn open_backend_log(app: &tauri::AppHandle) -> Option<std::fs::File> {
    let log_path = data_dir(app).join(BACKEND_LOG_FILE);
    if let Some(parent) = log_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "[dss-tauri] WARNING: failed to create log dir {}: {e}",
                parent.display()
            );
            return None;
        }
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!(
                "[dss-tauri] WARNING: failed to open backend log {}: {e}",
                log_path.display()
            );
            None
        }
    }
}

/// spawn 后端，返回 child handle。
/// Unix 下把子进程放到独立进程组，便于关窗口时整组清理。
fn spawn_backend(port: u16, binary: &std::path::Path, log: Option<std::fs::File>) -> Option<std::process::Child> {
    let stdout = log.as_ref().and_then(|f| f.try_clone().ok()).map(Stdio::from);
    let stderr = log.map(Stdio::from);

    let mut cmd = Command::new(binary);
    cmd.arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null());

    if let Some(s) = stdout {
        cmd.stdout(s);
    } else {
        cmd.stdout(Stdio::null());
    }
    if let Some(s) = stderr {
        cmd.stderr(s);
    } else {
        cmd.stderr(Stdio::null());
    }

    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let child = cmd.spawn().ok();
    if child.is_none() {
        eprintln!(
            "[dss-tauri] WARNING: failed to spawn backend at {}",
            binary.display()
        );
    }
    child
}



/// 清理后端进程组。
#[cfg(unix)]
fn kill_backend_child(child: &mut std::process::Child) {
    let pid = child.id() as i32;
    unsafe {
        // 先优雅 SIGTERM
        let _ = libc::killpg(pid, libc::SIGTERM);
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let _ = child.try_wait();
    unsafe {
        // 确保杀死
        let _ = libc::killpg(pid, libc::SIGKILL);
    }
    let _ = child.wait();
}

#[cfg(not(unix))]
fn kill_backend_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let port = find_free_port(DEFAULT_PORT);
            let binary = backend_binary_path(&app.handle());
            let log = open_backend_log(&app.handle());

            eprintln!(
                "[dss-tauri] starting backend: {} on port {port}",
                binary.display()
            );

            // spawn 后端
            let child = spawn_backend(port, &binary, log);

            // 等后端就绪（轮询 /api/health，最多 10 秒）
            let health_url = format!("http://127.0.0.1:{port}/api/health");
            let mut ready = false;
            for _ in 0..50 {
                if reqwest::blocking::get(&health_url).is_ok() {
                    ready = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            if !ready {
                eprintln!("[dss-tauri] WARNING: backend did not become healthy within 10s");
            }

            // 在主窗口页面加载前注入后端端口，确保前端脚本能读到。
            let init_script = format!(
                "window.__BACKEND_PORT__ = {port};
                 localStorage.setItem('dss_backend_port', '{port}');
                 console.log('[dss-tauri] backend port injected: {port}');"
            );
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("Deepseek Science")
            .inner_size(1280.0, 800.0)
            .min_inner_size(900.0, 600.0)
            .resizable(true)
            .fullscreen(false)
            .initialization_script(&init_script)
            .build()?;

            // 把 child 存进 app state，关窗时杀。
            if let Some(child) = child {
                app.manage(BackendChild {
                    child: std::sync::Mutex::new(child),
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if let Some(state) = window.app_handle().try_state::<BackendChild>() {
                    if let Ok(mut child) = state.child.lock() {
                        kill_backend_child(&mut child);
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

struct BackendChild {
    child: std::sync::Mutex<std::process::Child>,
}
