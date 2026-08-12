// Deepseek Science Tauri 壳：拉起后端二进制 + 注入端口 + 加载前端。
//
// 职责（modules.md §12 / tech-stack Tauri 栈）：
// 1. 找空闲端口（默认 17896）
// 2. spawn 后端二进制 dss-backend serve --port <port>
// 3. 把端口注入 webview（localStorage dss_backend_port + window.__BACKEND_PORT__）
// 4. 关窗时杀后端进程组，并把后端 stdout/stderr 落到日志文件

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Manager;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const DEFAULT_PORT: u16 = 17896;
const BACKEND_LOG_FILE: &str = "logs/backend.log";
const BACKEND_BINARY_NAME: &str = "dss-backend";
// Cold starts can be delayed by macOS code-signature and filesystem checks. Keep enough
// headroom that an already-listening backend is not misreported as dead by the shell.
const BACKEND_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const BACKEND_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Per-launch capability for the privileged localhost API (~244 random bits).
fn generate_api_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

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
fn data_dir(app: &tauri::AppHandle) -> PathBuf {
    if let Some(path) = data_dir_override() {
        return path;
    }
    app.path()
        .home_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".deepseek-science")
}

fn data_dir_override() -> Option<PathBuf> {
    std::env::var_os("DSS_DATA_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// 解析后端二进制路径：
/// - 打包：只接受映射到 Tauri 资源根目录的 `dss-backend`；
/// - 开发：使用编译期 `CARGO_MANIFEST_DIR`，确定性地解析根 workspace 的 debug 产物。
fn backend_binary_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let mut checked = Vec::new();

    if let Ok(dir) = app.path().resource_dir() {
        let candidate = dir.join(BACKEND_BINARY_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
        checked.push(candidate);
    }

    // Release 包必须自包含，不能悄悄回退到打包机的源码目录。
    if cfg!(debug_assertions) {
        let candidate = development_backend_binary_path(Path::new(env!("CARGO_MANIFEST_DIR")));
        if candidate.is_file() {
            return Ok(candidate);
        }
        checked.push(candidate);
    }

    let checked = checked
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!("backend binary not found (checked: {checked})"))
}

fn development_backend_binary_path(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .parent()
        .unwrap_or(manifest_dir)
        .join("target")
        .join("debug")
        .join(BACKEND_BINARY_NAME)
}

fn backend_log_path(app: &tauri::AppHandle) -> PathBuf {
    data_dir(app).join(BACKEND_LOG_FILE)
}

/// 打开后端日志文件，返回可写的 File。
fn open_backend_log(log_path: &Path) -> Option<std::fs::File> {
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
        .open(log_path)
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
fn spawn_backend(
    port: u16,
    binary: &Path,
    log: Option<std::fs::File>,
    api_token: &str,
    data_dir_override: Option<&Path>,
) -> Result<Child, String> {
    let stdout = log
        .as_ref()
        .and_then(|f| f.try_clone().ok())
        .map(Stdio::from);
    let stderr = log.map(Stdio::from);

    let mut cmd = Command::new(binary);
    configure_backend_command(
        &mut cmd,
        port,
        api_token,
        std::process::id(),
        data_dir_override,
    );
    cmd.stdin(Stdio::null());

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
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }

    cmd.spawn()
        .map_err(|e| format!("failed to spawn backend at {}: {e}", binary.display()))
}

/// Configure the packaged backend as a localhost-only child of this exact app process.
///
/// The hidden parent pid lets the backend stop itself if the GUI is force-killed, where
/// `BackendChild::drop` and the normal close handler cannot run. Explicitly pinning `DSS_HOST`
/// prevents an inherited shell environment from accidentally exposing the privileged API on
/// another interface.
fn configure_backend_command(
    cmd: &mut Command,
    port: u16,
    api_token: &str,
    parent_pid: u32,
    data_dir_override: Option<&Path>,
) {
    cmd.arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .arg("--parent-pid")
        .arg(parent_pid.to_string())
        .env("DSS_HOST", "127.0.0.1")
        .env("DSS_API_TOKEN", api_token);

    if let Some(data_dir) = data_dir_override {
        // Do not rely on LaunchServices/Command environment inheritance for isolation: the
        // exact directory selected by the desktop shell must also reach its backend child.
        cmd.env("DSS_DATA_DIR", data_dir);
    } else {
        // An inherited empty DSS_DATA_DIR is a real path (the current directory) to the
        // backend. Remove it so normal launches retain the documented home-directory default.
        cmd.env_remove("DSS_DATA_DIR");
    }
}

/// 等后端健康；若子进程提前退出，立即返回具体状态。
fn wait_for_backend(child: &mut Child, port: u16) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        // The health endpoint is always loopback. System or inherited proxy settings must
        // never turn this readiness probe into an external/proxied request.
        .no_proxy()
        .connect_timeout(Duration::from_millis(300))
        .timeout(Duration::from_millis(700))
        .build()
        .map_err(|e| format!("failed to build backend health client: {e}"))?;
    let health_url = format!("http://127.0.0.1:{port}/api/health");
    let deadline = Instant::now() + BACKEND_STARTUP_TIMEOUT;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!("backend exited before becoming healthy ({status})"));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("failed to inspect backend process: {e}")),
        }

        if client
            .get(&health_url)
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "backend did not become healthy within {} seconds",
                BACKEND_STARTUP_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 前端初始化脚本。启动失败时用独立错误页覆盖 React 根节点，避免静默展示坏掉的 UI。
fn initialization_script(port: u16, startup_error: Option<&str>, api_token: &str) -> String {
    let startup_error = serde_json::to_string(&startup_error).unwrap_or_else(|_| "null".into());
    let api_token = serde_json::to_string(api_token).unwrap_or_else(|_| "null".into());
    format!(
        r#"
window.__BACKEND_PORT__ = {port};
localStorage.setItem('dss_backend_port', '{port}');
window.__DSS_API_TOKEN__ = {api_token};
window.__DSS_BACKEND_STARTUP_ERROR__ = {startup_error};

if (window.__DSS_BACKEND_STARTUP_ERROR__) {{
  const renderBackendStartupError = () => {{
    document.title = 'Deepseek Science — Startup Error';
    document.documentElement.style.background = '#f7f8fc';
    document.body.style.margin = '0';

    const root = document.createElement('main');
    root.setAttribute('role', 'alert');
    root.style.cssText = 'min-height:100vh;box-sizing:border-box;display:flex;align-items:center;justify-content:center;padding:32px;background:#f7f8fc;color:#1f2329;font:14px -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif';

    const card = document.createElement('section');
    card.style.cssText = 'width:min(680px,100%);box-sizing:border-box;border:1px solid #dfe2ea;border-radius:12px;background:#fff;padding:28px';

    const badge = document.createElement('div');
    badge.textContent = 'BACKEND STARTUP FAILED';
    badge.style.cssText = 'margin-bottom:12px;color:#d64545;font-size:11px;font-weight:700;letter-spacing:.08em';

    const heading = document.createElement('h1');
    heading.textContent = 'Deepseek Science 无法启动后端';
    heading.style.cssText = 'margin:0 0 12px;font-size:20px;line-height:1.35';

    const detail = document.createElement('pre');
    detail.textContent = window.__DSS_BACKEND_STARTUP_ERROR__;
    detail.style.cssText = 'margin:0;white-space:pre-wrap;overflow-wrap:anywhere;border-radius:8px;background:#f7f8fc;padding:14px;color:#4b5563;font:12px ui-monospace,SFMono-Regular,Menlo,monospace;line-height:1.55';

    const hint = document.createElement('p');
    hint.textContent = '请关闭应用，检查上述后端日志后重试。';
    hint.style.cssText = 'margin:14px 0 0;color:#667085;font-size:13px';

    card.append(badge, heading, detail, hint);
    root.append(card);
    document.body.replaceChildren(root);
  }};

  if (document.readyState === 'loading') {{
    document.addEventListener('DOMContentLoaded', renderBackendStartupError, {{ once: true }});
  }} else {{
    renderBackendStartupError();
  }}
}} else {{
  console.log('[dss-tauri] backend port injected: {port}');
}}
"#
    )
}

/// 清理后端进程组。
#[cfg(unix)]
fn kill_backend_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let pid = child.id() as i32;
    unsafe {
        // 先优雅 SIGTERM
        let _ = libc::killpg(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + BACKEND_SHUTDOWN_TIMEOUT;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe {
        // 确保杀死
        let _ = libc::killpg(pid, libc::SIGKILL);
    }
    let _ = child.wait();
}

#[cfg(not(unix))]
fn kill_backend_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let port = find_free_port(DEFAULT_PORT);
            let api_token = generate_api_token();
            let data_dir_override = data_dir_override();
            let log_path = backend_log_path(app.handle());
            let mut child = None;

            let startup_error = match backend_binary_path(app.handle()) {
                Ok(binary) => {
                    eprintln!(
                        "[dss-tauri] starting backend: {} on port {port}",
                        binary.display()
                    );
                    match spawn_backend(
                        port,
                        &binary,
                        open_backend_log(&log_path),
                        &api_token,
                        data_dir_override.as_deref(),
                    ) {
                        Ok(mut spawned) => {
                            let readiness = wait_for_backend(&mut spawned, port);
                            child = Some(spawned);
                            readiness.err()
                        }
                        Err(error) => Some(error),
                    }
                }
                Err(error) => Some(error),
            }
            .map(|error| format!("{error}\nBackend log: {}", log_path.display()));

            if let Some(error) = &startup_error {
                eprintln!("[dss-tauri] ERROR: {error}");
            }

            // 先托管 child；若后续窗口构建失败，BackendChild::drop 仍会清理进程。
            if let Some(child) = child {
                app.manage(BackendChild::new(child));
            }

            // 在页面加载前注入端口；后端失败时初始化脚本会渲染可见错误页。
            let init_script = initialization_script(port, startup_error.as_deref(), &api_token);
            let window_title = if startup_error.is_some() {
                "Deepseek Science — Startup Error"
            } else {
                "Deepseek Science"
            };
            let mut window_builder = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title(window_title)
            .inner_size(1280.0, 800.0)
            .min_inner_size(900.0, 600.0)
            .resizable(true)
            .fullscreen(false)
            .initialization_script(&init_script);

            // macOS: 隐藏原生标题栏，让内容延伸到顶部；红黄绿三点悬浮在内容之上。
            // 前端会为左上角三点预留安全区，并提供可拖拽区域移动窗口。
            #[cfg(target_os = "macos")]
            {
                window_builder = window_builder
                    .title_bar_style(tauri::TitleBarStyle::Overlay)
                    .hidden_title(true);
            }

            window_builder.build()?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if let Some(state) = window.app_handle().try_state::<BackendChild>() {
                    state.terminate();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

struct BackendChild {
    child: Mutex<Option<Child>>,
}

impl BackendChild {
    fn new(child: Child) -> Self {
        Self {
            child: Mutex::new(Some(child)),
        }
    }

    fn terminate(&self) {
        let child = {
            let mut slot = self
                .child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            slot.take()
        };
        if let Some(mut child) = child {
            kill_backend_child(&mut child);
        }
    }
}

impl Drop for BackendChild {
    fn drop(&mut self) {
        let slot = self
            .child
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(mut child) = slot.take() {
            kill_backend_child(&mut child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_backend_path_uses_workspace_debug_target() {
        let path = development_backend_binary_path(Path::new("/repo/src-tauri"));
        assert_eq!(path, Path::new("/repo/target/debug/dss-backend"));
    }

    #[test]
    fn startup_error_is_serialized_into_initialization_script() {
        let script = initialization_script(
            17896,
            Some("spawn failed: \"permission denied\""),
            "test-token",
        );
        assert!(script.contains("spawn failed: \\\"permission denied\\\""));
        assert!(script.contains("renderBackendStartupError"));
    }

    #[test]
    fn api_token_is_json_serialized_and_never_persisted() {
        let token = "token\";\nwindow.injected = true;//";
        let encoded = serde_json::to_string(token).unwrap();
        let script = initialization_script(17896, None, token);

        assert!(script.contains(&format!("window.__DSS_API_TOKEN__ = {encoded};")));
        assert!(!script.contains("token\";\nwindow.injected = true;//"));
        assert!(!script.contains("localStorage.setItem('dss_api_token'"));
    }

    #[test]
    fn generated_api_token_is_high_entropy_hex() {
        let first = generate_api_token();
        let second = generate_api_token();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn packaged_backend_is_parent_bound_and_loopback_only() {
        use std::ffi::OsStr;

        let mut command = Command::new("/tmp/dss-backend");
        configure_backend_command(
            &mut command,
            17901,
            "launch-token",
            4242,
            Some(Path::new("/private/tmp/dss-isolated")),
        );

        let args = command.get_args().collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                OsStr::new("serve"),
                OsStr::new("--port"),
                OsStr::new("17901"),
                OsStr::new("--parent-pid"),
                OsStr::new("4242"),
            ]
        );
        let env = command
            .get_envs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            env.get(OsStr::new("DSS_HOST")),
            Some(&Some(OsStr::new("127.0.0.1")))
        );
        assert_eq!(
            env.get(OsStr::new("DSS_API_TOKEN")),
            Some(&Some(OsStr::new("launch-token")))
        );
        assert_eq!(
            env.get(OsStr::new("DSS_DATA_DIR")),
            Some(&Some(OsStr::new("/private/tmp/dss-isolated")))
        );
    }

    #[test]
    fn packaged_backend_removes_missing_data_dir_override() {
        use std::ffi::OsStr;

        let mut command = Command::new("/tmp/dss-backend");
        command.env("DSS_DATA_DIR", "/private/tmp/stale");
        configure_backend_command(&mut command, 17901, "launch-token", 4242, None);

        let env = command
            .get_envs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(env.get(OsStr::new("DSS_DATA_DIR")), Some(&None));
    }
}
