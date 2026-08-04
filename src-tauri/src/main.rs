// Deepseek Science Tauri 壳：拉起后端二进制 + 注入端口 + 加载前端。
//
// 职责（modules.md §12 / tech-stack Tauri 栈）：
// 1. 找空闲端口（默认 17896）
// 2. spawn 后端二进制 dss-backend serve --port <port>
// 3. 把端口注入 webview（localStorage dss_backend_port + window.__BACKEND_PORT__）
// 4. 关窗时杀进程组

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::{Command, Stdio};
use tauri::{Manager, WebviewWindow};

const DEFAULT_PORT: u16 = 17896;

/// 找一个可用端口：优先 DEFAULT_PORT，被占则递增。
fn find_free_port(start: u16) -> u16 {
    for port in start..start + 100 {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    start
}

/// 解析后端二进制路径：
/// - 开发：项目根 target/debug/dss-backend
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
    workspace_root.join("target").join("debug").join("dss-backend")
}

/// spawn 后端，返回 child handle。
fn spawn_backend(port: u16, binary: &std::path::Path) -> Option<std::process::Child> {
    let child = Command::new(binary)
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();
    if child.is_none() {
        eprintln!("[dss-tauri] WARNING: failed to spawn backend at {}", binary.display());
    }
    child
}

/// 把端口注入 webview（在页面加载前执行 init script）。
fn inject_port(window: &WebviewWindow, port: u16) {
    let script = format!(
        "window.__BACKEND_PORT__ = {port};
         localStorage.setItem('dss_backend_port', '{port}');
         console.log('[dss-tauri] backend port injected: {port}');"
    );
    let _ = window.eval(&script);
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let port = find_free_port(DEFAULT_PORT);
            let binary = backend_binary_path(&app.handle());

            // spawn 后端
            let child = spawn_backend(port, &binary);

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

            // 注入端口到主窗口
            if let Some(window) = app.get_webview_window("main") {
                inject_port(&window, port);
            }

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
                // 杀后端进程
                if let Some(state) = window.app_handle().try_state::<BackendChild>() {
                    if let Ok(mut child) = state.child.lock() {
                        let _ = child.kill();
                        let _ = child.wait();
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
