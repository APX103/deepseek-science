//! Fail-closed workspace process isolation for model-controlled shell and Python code.

// The non-macOS build intentionally exposes only fail-closed stubs; the concrete
// sandbox runtime below is retained there solely for cross-platform unit tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
#[cfg(target_os = "macos")]
use tokio::process::Command;

use crate::error::ToolError;
#[cfg(target_os = "macos")]
use crate::process::run_bounded;
use crate::process::ProcessOutput;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const CLEAN_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

// `system.sb` provides the private-but-required Darwin runtime/IPC baseline. Everything else
// remains denied unless explicitly allowed below. Child processes inherit the same sandbox.
#[cfg(target_os = "macos")]
const CODE_PROFILE: &str = r#"
(version 1)
(deny default)
(import "system.sb")
(allow process-fork)
(allow process-exec)
(allow signal (target same-sandbox))
(allow file-read-metadata)
(allow file-read* file-test-existence file-map-executable
    (subpath (param "WORKSPACE"))
    (subpath "/bin")
    (subpath "/sbin")
    (subpath "/usr/bin")
    (subpath "/usr/sbin")
    (subpath "/usr/lib")
    (subpath "/usr/share")
    (subpath "/System")
    (subpath "/Library/Apple")
    (subpath "/Library/Developer/CommandLineTools")
    (subpath "/Library/Python")
    (subpath "/Applications/Xcode.app")
    (subpath "/private/etc")
    (subpath "/private/var/db/timezone")
    (subpath "/private/var/select")
    (subpath "/opt/homebrew/bin")
    (subpath "/opt/homebrew/lib")
    (subpath "/opt/homebrew/Cellar")
    (subpath "/opt/homebrew/opt")
    (subpath "/opt/homebrew/share")
    (subpath "/usr/local/bin")
    (subpath "/usr/local/lib")
    (subpath "/usr/local/Cellar")
    (subpath "/usr/local/opt")
    (subpath "/usr/local/share")
    (subpath (param "PYTHON_PREFIX"))
    (subpath (param "PYTHON_SITE"))
    (literal (param "EXECUTABLE")))
(allow file-write* (subpath (param "WORKSPACE")))
(deny network*)
"#;

#[cfg(target_os = "macos")]
const TECTONIC_PROFILE: &str = r#"
(version 1)
(deny default)
(import "system.sb")
(allow process-fork)
(allow process-exec)
(allow signal (target same-sandbox))
; reqwest's macOS proxy discovery constructs an SCDynamicStore even in
; `--only-cached` mode. Permit that read-only system configuration IPC while
; keeping every network operation explicitly denied below.
(allow mach-lookup (global-name "com.apple.SystemConfiguration.configd"))
(allow file-read-metadata)
(allow file-read* file-test-existence file-map-executable
    (subpath (param "WORKSPACE"))
    (subpath (param "TECTONIC_CACHE"))
    (literal (param "EXECUTABLE"))
    (subpath "/bin")
    (subpath "/sbin")
    (subpath "/usr/bin")
    (subpath "/usr/sbin")
    (subpath "/usr/lib")
    (subpath "/usr/share")
    (subpath "/System")
    (subpath "/Library/Apple")
    (subpath "/private/etc")
    (subpath "/private/var/db/timezone")
    (subpath "/private/var/select")
    (subpath "/opt/homebrew/bin")
    (subpath "/opt/homebrew/lib")
    (subpath "/opt/homebrew/Cellar")
    (subpath "/opt/homebrew/opt")
    (subpath "/opt/homebrew/share")
    (subpath "/usr/local/bin")
    (subpath "/usr/local/lib")
    (subpath "/usr/local/Cellar")
    (subpath "/usr/local/opt")
    (subpath "/usr/local/share"))
(allow file-write* (subpath (param "WORKSPACE")) (subpath (param "TECTONIC_CACHE")))
(deny network*)
"#;

static RUNTIME_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct WorkspaceRuntime {
    root: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
}

impl WorkspaceRuntime {
    fn create(workspace: &Path) -> Result<Self, ToolError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..32 {
            let sequence = RUNTIME_COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = workspace.join(format!(
                ".dss-sandbox-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            match std::fs::create_dir(&root) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
                    }
                    let home = root.join("home");
                    let tmp = root.join("tmp");
                    std::fs::create_dir(&home)?;
                    std::fs::create_dir(&tmp)?;
                    return Ok(Self { root, home, tmp });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(ToolError::other(
            "could not create an isolated runtime directory in the workspace",
        ))
    }
}

impl Drop for WorkspaceRuntime {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, Deserialize)]
struct PythonProbe {
    executable: PathBuf,
    prefix: PathBuf,
    version: String,
}

#[derive(Debug)]
struct PythonRuntime {
    executable: PathBuf,
    prefix: PathBuf,
    user_base: Option<PathBuf>,
    user_site: PathBuf,
}

pub(crate) async fn run_workspace_shell(
    workspace: &Path,
    source: &str,
    timeout: Duration,
) -> Result<ProcessOutput, ToolError> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (workspace, source, timeout);
        Err(unsupported_platform())
    }

    #[cfg(target_os = "macos")]
    {
        let workspace = canonical_workspace(workspace)?;
        let shell = PathBuf::from("/bin/sh");
        require_executable(Path::new(SANDBOX_EXEC), "macOS sandbox-exec")?;
        require_executable(&shell, "system shell")?;
        run_macos_sandboxed(
            Path::new(SANDBOX_EXEC),
            &workspace,
            &shell,
            &["-c", source],
            &workspace,
            None,
            timeout,
        )
        .await
    }
}

pub(crate) async fn run_workspace_python(
    workspace: &Path,
    source: &str,
    timeout: Duration,
) -> Result<ProcessOutput, ToolError> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (workspace, source, timeout);
        Err(unsupported_platform())
    }

    #[cfg(target_os = "macos")]
    {
        let workspace = canonical_workspace(workspace)?;
        require_executable(Path::new(SANDBOX_EXEC), "macOS sandbox-exec")?;
        let python = resolve_python(&workspace).await?;
        run_macos_sandboxed(
            Path::new(SANDBOX_EXEC),
            &workspace,
            &python.executable,
            &["-c", source],
            &python.prefix,
            Some(&python),
            timeout,
        )
        .await
    }
}

pub(crate) async fn run_workspace_tectonic(
    workspace: &Path,
    executable: &Path,
    cache: &Path,
    home: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<ProcessOutput, ToolError> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (workspace, executable, cache, home, args, timeout);
        Err(unsupported_platform())
    }

    #[cfg(target_os = "macos")]
    {
        let workspace = canonical_workspace(workspace)?;
        let executable = executable.canonicalize().map_err(|error| {
            ToolError::other(format!(
                "Tectonic sandbox unavailable: could not resolve {}: {error}",
                executable.display()
            ))
        })?;
        require_executable(Path::new(SANDBOX_EXEC), "macOS sandbox-exec")?;
        require_executable(&executable, "Tectonic executable")?;
        let cache = cache.canonicalize().map_err(|error| {
            ToolError::other(format!(
                "Tectonic sandbox unavailable: could not resolve cache {}: {error}",
                cache.display()
            ))
        })?;
        if !cache.is_dir() {
            return Err(ToolError::other(format!(
                "Tectonic sandbox unavailable: cache is not a directory: {}",
                cache.display()
            )));
        }
        let home = home.canonicalize().map_err(|error| {
            ToolError::other(format!(
                "Tectonic sandbox unavailable: could not resolve HOME {}: {error}",
                home.display()
            ))
        })?;
        run_macos_tectonic(&workspace, &executable, &cache, &home, args, timeout).await
    }
}

#[cfg(not(target_os = "macos"))]
fn unsupported_platform() -> ToolError {
    ToolError::other(
        "process tools are disabled: no supported workspace sandbox is available on this platform",
    )
}

#[cfg(target_os = "macos")]
fn canonical_workspace(workspace: &Path) -> Result<PathBuf, ToolError> {
    let workspace = workspace.canonicalize().map_err(|error| {
        ToolError::other(format!(
            "workspace sandbox unavailable: could not resolve {}: {error}",
            workspace.display()
        ))
    })?;
    if !workspace.is_dir() {
        return Err(ToolError::other(format!(
            "workspace sandbox unavailable: {} is not a directory",
            workspace.display()
        )));
    }
    require_utf8(&workspace, "workspace")?;
    Ok(workspace)
}

#[cfg(target_os = "macos")]
async fn resolve_python(workspace: &Path) -> Result<PythonRuntime, ToolError> {
    const PROBE: &str = r#"import json, os, sys
print(json.dumps({"executable": os.path.realpath(sys.executable), "prefix": os.path.realpath(sys.prefix), "version": f"{sys.version_info.major}.{sys.version_info.minor}"}))"#;
    let candidates: [(&str, &[&str]); 3] = [
        (
            "/usr/bin/python3",
            &[
                "/usr/bin",
                "/Applications/Xcode.app",
                "/Library/Developer/CommandLineTools",
            ],
        ),
        ("/opt/homebrew/bin/python3", &["/opt/homebrew"]),
        ("/usr/local/bin/python3", &["/usr/local"]),
    ];
    let mut failures = Vec::new();
    for (launcher, trusted_root_names) in candidates {
        let launcher = Path::new(launcher);
        if !launcher.is_file() {
            continue;
        }
        let trusted_roots: Vec<PathBuf> = trusted_root_names
            .iter()
            .filter_map(|root| Path::new(root).canonicalize().ok())
            .collect();
        let launcher = match launcher.canonicalize() {
            Ok(path) if path.is_file() && is_within_roots(&path, &trusted_roots) => path,
            Ok(path) => {
                failures.push(format!(
                    "{} is outside its trusted installation root",
                    path.display()
                ));
                continue;
            }
            Err(error) => {
                failures.push(format!("{}: {error}", launcher.display()));
                continue;
            }
        };
        let launcher_root = trusted_roots
            .iter()
            .find(|root| launcher.starts_with(root))
            .expect("launcher was checked against trusted roots");
        let output = match run_macos_sandboxed(
            Path::new(SANDBOX_EXEC),
            workspace,
            &launcher,
            &["-I", "-S", "-c", PROBE],
            launcher_root,
            None,
            Duration::from_secs(5),
        )
        .await
        {
            Ok(output)
                if output
                    .status
                    .as_ref()
                    .is_some_and(|status| status.success()) =>
            {
                output
            }
            Ok(output) => {
                failures.push(format!(
                    "{} discovery exited {:?}",
                    launcher.display(),
                    output.status.and_then(|status| status.code())
                ));
                continue;
            }
            Err(error) => {
                failures.push(format!("{}: {error}", launcher.display()));
                continue;
            }
        };
        let probe: PythonProbe = match serde_json::from_slice(&output.stdout) {
            Ok(probe) => probe,
            Err(error) => {
                failures.push(format!(
                    "{} returned invalid metadata: {error}",
                    launcher.display()
                ));
                continue;
            }
        };
        let executable = match probe.executable.canonicalize() {
            Ok(path) if path.is_file() && is_within_roots(&path, &trusted_roots) => path,
            Ok(path) => {
                failures.push(format!(
                    "{} is not a trusted Python executable",
                    path.display()
                ));
                continue;
            }
            Err(error) => {
                failures.push(format!("{}: {error}", probe.executable.display()));
                continue;
            }
        };
        let prefix = match probe.prefix.canonicalize() {
            Ok(path) if path.is_dir() && is_within_roots(&path, &trusted_roots) => path,
            Ok(path) => {
                failures.push(format!("{} is not a trusted Python prefix", path.display()));
                continue;
            }
            Err(error) => {
                failures.push(format!("{}: {error}", probe.prefix.display()));
                continue;
            }
        };
        require_utf8(&executable, "Python executable")?;
        require_utf8(&prefix, "Python prefix")?;

        let (user_base, user_site) = resolve_user_site(&probe.version)
            .map(|(base, site)| (Some(base), site))
            .unwrap_or_else(|| (None, prefix.clone()));
        require_utf8(&user_site, "Python site-packages")?;
        return Ok(PythonRuntime {
            executable,
            prefix,
            user_base,
            user_site,
        });
    }

    Err(ToolError::other(format!(
        "python tool unavailable: no trusted Python 3 interpreter could be resolved{}",
        if failures.is_empty() {
            String::new()
        } else {
            format!(" ({})", failures.join("; "))
        }
    )))
}

#[cfg(target_os = "macos")]
fn is_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

#[cfg(target_os = "macos")]
fn resolve_user_site(version: &str) -> Option<(PathBuf, PathBuf)> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let python_root = home.join("Library/Python").canonicalize().ok()?;
    let user_base = python_root.join(version).canonicalize().ok()?;
    if !user_base.starts_with(&python_root) {
        return None;
    }
    let user_site = user_base
        .join("lib/python/site-packages")
        .canonicalize()
        .ok()?;
    if !user_site.starts_with(&user_base) || !user_site.is_dir() {
        return None;
    }
    Some((user_base, user_site))
}

#[cfg(target_os = "macos")]
async fn run_macos_sandboxed(
    sandbox_exec: &Path,
    workspace: &Path,
    executable: &Path,
    args: &[&str],
    python_prefix: &Path,
    python: Option<&PythonRuntime>,
    timeout: Duration,
) -> Result<ProcessOutput, ToolError> {
    let runtime = WorkspaceRuntime::create(workspace)?;
    let workspace = require_utf8(workspace, "workspace")?;
    let executable_param = require_utf8(executable, "executable")?;
    let python_prefix = require_utf8(python_prefix, "Python prefix")?;
    let python_site = require_utf8(
        python
            .map(|runtime| runtime.user_site.as_path())
            .unwrap_or(executable),
        "Python site-packages",
    )?;
    let home = require_utf8(&runtime.home, "sandbox HOME")?;
    let tmp = require_utf8(&runtime.tmp, "sandbox TMPDIR")?;

    require_executable(sandbox_exec, "macOS sandbox-exec")?;
    let mut command = Command::new(sandbox_exec);
    command
        .arg("-D")
        .arg(format!("WORKSPACE={workspace}"))
        .arg("-D")
        .arg(format!("EXECUTABLE={executable_param}"))
        .arg("-D")
        .arg(format!("PYTHON_PREFIX={python_prefix}"))
        .arg("-D")
        .arg(format!("PYTHON_SITE={python_site}"))
        .arg("-p")
        .arg(CODE_PROFILE)
        .arg(executable)
        .args(args)
        .current_dir(workspace)
        .env_clear()
        .env("PATH", CLEAN_PATH)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("HOME", home)
        .env("TMPDIR", tmp);
    if let Some(user_base) = python.and_then(|runtime| runtime.user_base.as_ref()) {
        command.env("PYTHONUSERBASE", user_base);
    }

    let result = run_bounded(command, timeout).await;
    drop(runtime);
    result
}

#[cfg(target_os = "macos")]
async fn run_macos_tectonic(
    workspace: &Path,
    executable: &Path,
    cache: &Path,
    home: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<ProcessOutput, ToolError> {
    let runtime = WorkspaceRuntime::create(workspace)?;
    let workspace = require_utf8(workspace, "workspace")?;
    let executable_param = require_utf8(executable, "Tectonic executable")?;
    let cache = require_utf8(cache, "Tectonic cache")?;
    let home = require_utf8(home, "Tectonic HOME")?;
    let tmp = require_utf8(&runtime.tmp, "sandbox TMPDIR")?;

    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-D")
        .arg(format!("WORKSPACE={workspace}"))
        .arg("-D")
        .arg(format!("EXECUTABLE={executable_param}"))
        .arg("-D")
        .arg(format!("TECTONIC_CACHE={cache}"))
        .arg("-p")
        .arg(TECTONIC_PROFILE)
        .arg(executable)
        .args(args)
        .current_dir(workspace)
        .env_clear()
        .env("PATH", CLEAN_PATH)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("HOME", home)
        .env("TMPDIR", tmp);

    let result = run_bounded(command, timeout).await;
    drop(runtime);
    result
}

#[cfg(target_os = "macos")]
fn require_executable(path: &Path, label: &str) -> Result<(), ToolError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(ToolError::other(format!(
            "workspace sandbox unavailable: {label} not found at {}",
            path.display()
        )))
    }
}

#[cfg(target_os = "macos")]
fn require_utf8<'a>(path: &'a Path, label: &str) -> Result<&'a str, ToolError> {
    path.to_str().ok_or_else(|| {
        ToolError::other(format!(
            "workspace sandbox unavailable: {label} path is not valid UTF-8"
        ))
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    struct TestTree {
        root: PathBuf,
        workspace: PathBuf,
        outside: PathBuf,
    }

    impl TestTree {
        fn new(label: &str) -> Self {
            let sequence = RUNTIME_COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = PathBuf::from("/tmp").join(format!(
                "dss-tools-sandbox-test-{label}-{}-{sequence}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            let workspace = root.join("workspace");
            let outside = root.join("outside");
            std::fs::create_dir_all(&workspace).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("sentinel.txt"), "outside-secret-sentinel").unwrap();
            Self {
                root,
                workspace,
                outside,
            }
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn success(output: &ProcessOutput) -> bool {
        output.status.as_ref().and_then(|status| status.code()) == Some(0)
    }

    #[tokio::test]
    async fn shell_can_write_workspace_but_not_read_or_write_sibling() {
        let tree = TestTree::new("shell-boundary");
        const TEST_ENV: &str = "DSS_SANDBOX_TEST_CREDENTIAL_DO_NOT_INHERIT";
        std::env::set_var(TEST_ENV, "fake-test-secret");
        let output = run_workspace_shell(
            &tree.workspace,
            r#"
printf workspace-ok > inside.txt
if value=$(/bin/cat ../outside/sentinel.txt 2>/dev/null); then
  printf 'outside_read=allowed:%s\n' "$value"
else
  printf 'outside_read=denied\n'
fi
if printf changed > ../outside/sentinel.txt 2>/dev/null; then
  printf 'outside_write=allowed\n'
else
  printf 'outside_write=denied\n'
fi
printf 'credential=%s\n' "${DSS_SANDBOX_TEST_CREDENTIAL_DO_NOT_INHERIT-unset}"
printf 'pythonpath=%s\n' "${PYTHONPATH-unset}"
printf 'home=%s\n' "$HOME"
printf 'tmp=%s\n' "$TMPDIR"
"#,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        std::env::remove_var(TEST_ENV);

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(success(&output), "stdout={stdout}\nstderr={stderr}");
        assert!(stdout.contains("outside_read=denied"), "{stdout}");
        assert!(stdout.contains("outside_write=denied"), "{stdout}");
        assert!(stdout.contains("credential=unset"), "{stdout}");
        assert!(stdout.contains("pythonpath=unset"), "{stdout}");
        assert_eq!(
            std::fs::read_to_string(tree.workspace.join("inside.txt")).unwrap(),
            "workspace-ok"
        );
        assert_eq!(
            std::fs::read_to_string(tree.outside.join("sentinel.txt")).unwrap(),
            "outside-secret-sentinel"
        );
        assert!(!stdout.contains("outside-secret-sentinel"));
        assert!(
            std::fs::read_dir(&tree.workspace)
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".dss-sandbox-")),
            "sandbox HOME/TMPDIR should be removed after execution"
        );
    }

    #[tokio::test]
    async fn python_supports_stdlib_and_available_science_packages_without_network() {
        let tree = TestTree::new("python-boundary");
        const TEST_ENV: &str = "DSS_SANDBOX_TEST_PYTHON_CREDENTIAL_DO_NOT_INHERIT";
        std::env::set_var(TEST_ENV, "fake-test-secret");
        let result = run_workspace_python(
            &tree.workspace,
            r#"
import importlib, importlib.util, os, pathlib, socket, tempfile
outside = pathlib.Path('../outside/sentinel.txt')
try:
    print('outside_read=allowed:' + outside.read_text())
except OSError as error:
    print('outside_read_errno=' + str(error.errno))
try:
    outside.write_text('changed')
    print('outside_write=allowed')
except OSError as error:
    print('outside_write_errno=' + str(error.errno))
pathlib.Path('python-inside.txt').write_text('python-ok')
with tempfile.NamedTemporaryFile() as temporary:
    print('temp_in_workspace=' + str(pathlib.Path(temporary.name).is_relative_to(pathlib.Path.cwd())))
for operation in ('bind', 'connect'):
    sock = socket.socket()
    try:
        if operation == 'bind':
            sock.bind(('127.0.0.1', 0))
        else:
            sock.connect(('127.0.0.1', 9))
        print(operation + '=allowed')
    except OSError as error:
        print(operation + '_errno=' + str(error.errno))
    finally:
        sock.close()
available = [name for name in ('numpy', 'scipy') if importlib.util.find_spec(name)]
for name in available:
    importlib.import_module(name)
print('science_imports=' + ','.join(available))
print('credential=' + str(os.environ.get('DSS_SANDBOX_TEST_PYTHON_CREDENTIAL_DO_NOT_INHERIT')))
print('pythonpath=' + str(os.environ.get('PYTHONPATH')))
"#,
            Duration::from_secs(20),
        )
        .await;
        std::env::remove_var(TEST_ENV);
        let output = result.unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(success(&output), "stdout={stdout}\nstderr={stderr}");
        assert!(stdout.contains("outside_read_errno=1"), "{stdout}");
        assert!(stdout.contains("outside_write_errno=1"), "{stdout}");
        assert!(stdout.contains("bind_errno=1"), "{stdout}");
        assert!(stdout.contains("connect_errno=1"), "{stdout}");
        assert!(stdout.contains("temp_in_workspace=True"), "{stdout}");
        assert!(stdout.contains("credential=None"), "{stdout}");
        assert!(stdout.contains("pythonpath=None"), "{stdout}");
        assert_eq!(
            std::fs::read_to_string(tree.workspace.join("python-inside.txt")).unwrap(),
            "python-ok"
        );
        assert_eq!(
            std::fs::read_to_string(tree.outside.join("sentinel.txt")).unwrap(),
            "outside-secret-sentinel"
        );
        assert!(!stdout.contains("outside-secret-sentinel"));
    }

    #[tokio::test]
    async fn missing_sandbox_fails_before_payload_runs() {
        let tree = TestTree::new("fail-closed");
        let marker = tree.workspace.join("must-not-exist.txt");
        let shell = PathBuf::from("/bin/sh");
        let error = run_macos_sandboxed(
            Path::new("/tmp/dss-tools-missing-sandbox-exec"),
            &tree.workspace.canonicalize().unwrap(),
            &shell,
            &["-c", "printf ran > must-not-exist.txt"],
            &tree.workspace.canonicalize().unwrap(),
            None,
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("sandbox-exec"));
        assert!(!marker.exists(), "payload ran despite unavailable sandbox");
    }

    #[tokio::test]
    async fn tectonic_profile_allows_only_workspace_and_exact_cache() {
        use std::os::unix::fs::PermissionsExt;

        let tree = TestTree::new("tectonic-boundary");
        let home = tree.root.join("home");
        let cache = home.join("Library/Caches/Tectonic");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("cache-sentinel.txt"), "cached-resource").unwrap();
        let fake_tectonic = tree.root.join("fake-tectonic");
        std::fs::write(
            &fake_tectonic,
            format!(
                r#"#!/bin/sh
if /bin/cat '{}' > leaked.txt 2>/dev/null; then
  printf outside-read-allowed
else
  printf outside-read-denied > outside-read.txt
fi
if printf changed > '{}' 2>/dev/null; then
  printf outside-write-allowed
else
  printf outside-write-denied > outside-write.txt
fi
/bin/cat '{}' > cache-read.txt
printf cache-write-ok > '{}'
printf compiled > fake-output.pdf
"#,
                tree.outside.join("sentinel.txt").display(),
                tree.outside.join("sentinel.txt").display(),
                cache.join("cache-sentinel.txt").display(),
                cache.join("cache-write.txt").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_tectonic, std::fs::Permissions::from_mode(0o700)).unwrap();

        let output = run_workspace_tectonic(
            &tree.workspace,
            &fake_tectonic,
            &cache,
            &home,
            &[],
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(success(&output), "stdout={stdout}\nstderr={stderr}");
        assert_eq!(
            std::fs::read_to_string(tree.workspace.join("outside-read.txt")).unwrap(),
            "outside-read-denied"
        );
        assert_eq!(
            std::fs::read_to_string(tree.workspace.join("outside-write.txt")).unwrap(),
            "outside-write-denied"
        );
        assert_eq!(
            std::fs::read_to_string(tree.workspace.join("cache-read.txt")).unwrap(),
            "cached-resource"
        );
        assert_eq!(
            std::fs::read_to_string(cache.join("cache-write.txt")).unwrap(),
            "cache-write-ok"
        );
        assert_eq!(
            std::fs::read_to_string(tree.outside.join("sentinel.txt")).unwrap(),
            "outside-secret-sentinel"
        );
        let leaked = std::fs::read(tree.workspace.join("leaked.txt")).unwrap_or_default();
        assert!(
            leaked.is_empty(),
            "outside sentinel contents escaped sandbox"
        );
    }
}
