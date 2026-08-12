use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// One canonical history checkpoint emitted by the Runner after a delivered tool batch. The API
/// acknowledges only after SQLite commits, preventing a long post-tool LLM turn from being the
/// sole copy of an already visible A2A trace.
pub struct HistoryCheckpoint {
    pub messages: Vec<dss_llm::ChatMessage>,
    pub frame_id: String,
    pub parent_frame_id: Option<String>,
    pub root_frame_id: Option<String>,
    pub agent_name: String,
    pub task_summary: String,
    pub plan: Option<PlanState>,
    pub pending_ask: Option<PendingAsk>,
    pub status: String,
    pub awaiting: Option<String>,
    pub ack: oneshot::Sender<Result<(), String>>,
}

pub struct HistoryCheckpointState {
    pub frame_id: String,
    pub parent_frame_id: Option<String>,
    pub root_frame_id: Option<String>,
    pub agent_name: String,
    pub task_summary: String,
    pub plan: Option<PlanState>,
    pub pending_ask: Option<PendingAsk>,
    pub status: String,
    pub awaiting: Option<String>,
}

/// ask_user 工具的一个候选项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAskOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// ask_user 工具挂起的提问。Runner 检测到后转 AwaitingUserResponse，
/// 并把此结构放进 complete.pending_ask 推给前端。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAsk {
    pub question: String,
    /// 候选项（可空，表示自由文本回复）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<PendingAskOption>,
    /// 短标题（前端展示用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
}

/// 工具运行时共享态。
///
/// P2a 最小集：workspace（文件工具根 + bash cwd）+ pending_ask（ask_user 挂起）。
/// 后续阶段按 modules.md 扩展（frame/artifact_store/venv/api_keys/skill_catalog/...）。
#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    /// Open directory capability for workspace file tools. Keeping the root fd alive for the
    /// lifetime of the context makes path confinement independent of later pathname changes.
    secure_workspace: Arc<Result<SecureWorkspace, String>>,
    /// Coordinates path-based sandboxed processes with dirfd-based file operations. In
    /// particular, compile holds the write side from source validation through output commit, so
    /// a concurrently requested Bash/Python tool cannot swap a validated path for a symlink.
    workspace_access: Arc<RwLock<()>>,
    pub pending_ask: Arc<Mutex<Option<PendingAsk>>>,
    /// skill 目录（P5a：让 search_skills/list_skills/skill 工具可用）。
    pub skill_catalog: Arc<dss_skills::SkillCatalog>,
    /// MCP server 管理器（P7：让 mcp__{server}__{tool} 动态工具转发）。
    pub mcp: Arc<dss_mcp::MCPServerManager>,
    /// Per-user-run circuit breaker for MCP tools whose remote annotations do not establish safe
    /// retry semantics. Reserve before network I/O because a timeout may follow a committed side
    /// effect. Cloned contexts share the guard; every newly accepted run creates a fresh context.
    mcp_mutation_attempts: Arc<Mutex<std::collections::HashSet<(String, String)>>>,
    /// plan 模式共享态（P6a：generate_plan 写入，Runner 读以转 awaiting）。
    pub plan: Arc<Mutex<Option<PlanState>>>,
    /// LLM 客户端 + 模型（P6b：delegate 工具用，做单次子任务 LLM 调用）。
    pub llm: Option<std::sync::Arc<dyn dss_llm::LlmClient>>,
    pub model: String,
    /// delegate 深度（modules.md 上限 2；主 agent 为 0，子为 1，孙为 2）。
    pub delegate_depth: u32,
    history_checkpoint_tx: Option<mpsc::Sender<HistoryCheckpoint>>,
    /// 记忆库（阶段二：search_memory/read_memory 工具用）。None = 记忆功能关闭。
    pub memory: Option<Arc<dss_memory::MemoryStore>>,
    /// 当前 project_id（记忆按项目隔离召回）。
    pub project_id: Option<String>,
    /// 数据源 API keys（OPENALEX_API_KEY 等）。工具按 key 名读取。
    pub api_keys: HashMap<String, String>,
}

/// plan 模式状态（generate_plan 产出）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanState {
    pub steps: Vec<PlanStep>,
    pub approved: bool,
    #[serde(default)]
    pub research_question: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanStep {
    pub title: String,
    pub status: String, // pending|running|done|failed
}

/// One entry discovered by [`SecureWorkspace::list`]. Symlinks and special files are never
/// returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// A capability-style handle to a workspace directory.
///
/// All untrusted relative paths are resolved one component at a time from `root`. On Unix each
/// step uses `openat` with `O_NOFOLLOW`; file replacement and deletion use `renameat`/`unlinkat`
/// relative to an already-open parent directory. Consequently a path component changed into a
/// symlink between validation and use is either rejected or operates on the directory inode that
/// was already opened -- it is never followed outside the workspace.
#[derive(Clone)]
pub struct SecureWorkspace {
    root: Arc<std::fs::File>,
    display_root: PathBuf,
    access: Arc<RwLock<()>>,
}

impl ToolContext {
    pub fn new(workspace: PathBuf) -> Self {
        // Open the capability eagerly. Delaying this until a tool call would leave a larger
        // pathname-race window and would make cloned contexts observe different roots.
        let secure_workspace = SecureWorkspace::open(&workspace)
            .map_err(|error| format!("cannot open workspace securely: {error}"));
        let workspace_access = secure_workspace
            .as_ref()
            .map(|workspace| workspace.access.clone())
            .unwrap_or_else(|_| Arc::new(RwLock::new(())));
        let secure_workspace = Arc::new(secure_workspace);
        Self {
            workspace,
            secure_workspace,
            workspace_access,
            pending_ask: Arc::new(Mutex::new(None)),
            skill_catalog: Arc::new(dss_skills::SkillCatalog::new()),
            mcp: Arc::new(dss_mcp::MCPServerManager::new()),
            mcp_mutation_attempts: Arc::new(Mutex::new(std::collections::HashSet::new())),
            plan: Arc::new(Mutex::new(None)),
            llm: None,
            model: String::new(),
            delegate_depth: 0,
            history_checkpoint_tx: None,
            memory: None,
            project_id: None,
            api_keys: HashMap::new(),
        }
    }

    pub fn with_skill_catalog(mut self, catalog: dss_skills::SkillCatalog) -> Self {
        self.skill_catalog = Arc::new(catalog);
        self
    }

    /// 注入 LLM 客户端 + 模型（delegate 工具用）。
    pub fn with_llm(mut self, llm: std::sync::Arc<dyn dss_llm::LlmClient>, model: String) -> Self {
        self.llm = Some(llm);
        self.model = model;
        self
    }

    /// 注入记忆库（阶段二：让 search_memory/read_memory 工具可用）。
    pub fn with_memory(
        mut self,
        memory: Arc<dss_memory::MemoryStore>,
        project_id: Option<String>,
    ) -> Self {
        self.memory = Some(memory);
        self.project_id = project_id;
        self
    }

    /// 注入数据源 API keys（OPENALEX_API_KEY 等，供 search_papers 等工具读取）。
    pub fn with_api_keys(mut self, api_keys: HashMap<String, String>) -> Self {
        self.api_keys = api_keys;
        self
    }

    pub fn with_mcp(mut self, mcp: dss_mcp::MCPServerManager) -> Self {
        self.mcp = Arc::new(mcp);
        self
    }

    /// 共享同一个 MCPServerManager（跨 session 复用连接态）。
    pub fn with_mcp_arc(mut self, mcp: Arc<dss_mcp::MCPServerManager>) -> Self {
        self.mcp = mcp;
        self
    }

    /// Atomically reserve the only permitted network attempt for one possibly-mutating MCP tool
    /// in this run. Returns false after any prior attempt, including one that timed out.
    pub async fn reserve_mcp_mutation_attempt(&self, server: &str, tool: &str) -> bool {
        self.mcp_mutation_attempts
            .lock()
            .await
            .insert((server.to_owned(), tool.to_owned()))
    }

    /// 注入已有的 plan 状态（P6：跨 run 恢复/同步）。
    pub async fn with_plan(self, plan: Option<PlanState>) -> Self {
        *self.plan.lock().await = plan;
        self
    }

    pub fn with_history_checkpoint(mut self, sender: mpsc::Sender<HistoryCheckpoint>) -> Self {
        self.history_checkpoint_tx = Some(sender);
        self
    }

    /// Returns after durable acknowledgement. Contexts used by unit tests and non-API callers
    /// have no sender and therefore retain the old no-op behavior.
    pub async fn checkpoint_history(
        &self,
        messages: Vec<dss_llm::ChatMessage>,
        state: HistoryCheckpointState,
    ) -> Result<(), String> {
        let Some(sender) = self.history_checkpoint_tx.as_ref() else {
            return Ok(());
        };
        if messages.is_empty() {
            return Ok(());
        }
        let HistoryCheckpointState {
            frame_id,
            parent_frame_id,
            root_frame_id,
            agent_name,
            task_summary,
            plan,
            pending_ask,
            status,
            awaiting,
        } = state;
        let (ack, receive_ack) = oneshot::channel();
        sender
            .send(HistoryCheckpoint {
                messages,
                frame_id,
                parent_frame_id,
                root_frame_id,
                agent_name,
                task_summary,
                plan,
                pending_ask,
                status,
                awaiting,
                ack,
            })
            .await
            .map_err(|_| "history checkpoint worker stopped".to_string())?;
        receive_ack
            .await
            .map_err(|_| "history checkpoint acknowledgement was dropped".to_string())?
    }

    /// Return the anchored workspace capability used by all built-in file tools.
    pub fn secure_workspace(&self) -> Result<SecureWorkspace, crate::error::ToolError> {
        self.secure_workspace
            .as_ref()
            .as_ref()
            .cloned()
            .map_err(|message| crate::error::ToolError::Other(message.clone()))
    }

    /// Shared guard for sandboxed Bash/Python processes. They may run in parallel with peers, but
    /// are excluded while compile/edit/write holds the validation-critical write side.
    pub async fn lock_workspace_read(&self) -> OwnedRwLockReadGuard<()> {
        self.workspace_access.clone().read_owned().await
    }

    /// Exclusive guard for path-based processes and multi-step mutations. The guard must cover
    /// validation, process execution, and final rename as one critical section.
    pub async fn lock_workspace_write(&self) -> OwnedRwLockWriteGuard<()> {
        self.workspace_access.clone().write_owned().await
    }

    /// Resolve a path for separately sandboxed subprocess launchers.
    ///
    /// A returned `PathBuf` cannot preserve a directory capability and must not be used as a
    /// security boundary for direct file I/O. File tools and HTTP file endpoints use
    /// [`Self::secure_workspace`] instead, so validation and access cannot be separated by a
    /// symlink race.
    pub fn resolve_in_workspace(&self, rel: &str) -> Result<PathBuf, crate::error::ToolError> {
        let trimmed = rel.trim();
        if trimmed.is_empty() {
            return Err(crate::error::ToolError::PathEscape("empty path".into()));
        }
        let workspace = self.workspace.canonicalize()?;
        let p = std::path::Path::new(trimmed);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            workspace.join(trimmed)
        };
        let norm = lexically_resolve(&abs);
        if !norm.starts_with(&workspace) {
            return Err(crate::error::ToolError::PathEscape(rel.into()));
        }

        // Existing targets are checked after resolving symlinks. For a new
        // target, canonicalize its nearest existing ancestor so a symlinked
        // parent cannot redirect a write outside the workspace.
        if norm.exists() {
            let canonical = norm.canonicalize()?;
            return canonical
                .starts_with(&workspace)
                .then_some(canonical)
                .ok_or_else(|| crate::error::ToolError::PathEscape(rel.into()));
        }

        let mut ancestor = norm.as_path();
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| crate::error::ToolError::PathEscape(rel.into()))?;
        }
        let canonical_ancestor = ancestor.canonicalize()?;
        if !canonical_ancestor.starts_with(&workspace) {
            return Err(crate::error::ToolError::PathEscape(rel.into()));
        }
        Ok(norm)
    }
}

impl SecureWorkspace {
    /// Open `workspace` as a directory capability. The final workspace component itself must not
    /// be a symlink. Workspace parents are trusted application configuration; all components
    /// below this fd are treated as untrusted.
    pub fn open(workspace: &Path) -> Result<Self, crate::error::ToolError> {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::io::FromRawFd;

            let path = CString::new(workspace.as_os_str().as_bytes()).map_err(|_| {
                crate::error::ToolError::InvalidArgs("workspace path contains NUL".into())
            })?;
            // O_NOFOLLOW rejects a workspace path whose final component has itself been swapped
            // for a symlink. The returned fd remains bound to this directory inode thereafter.
            let fd = unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if fd < 0 {
                return Err(map_workspace_io(
                    &workspace.to_string_lossy(),
                    std::io::Error::last_os_error(),
                ));
            }
            let root = unsafe { std::fs::File::from_raw_fd(fd) };
            let access = shared_workspace_access(&root)?;
            Ok(Self {
                root: Arc::new(root),
                display_root: workspace.to_path_buf(),
                access,
            })
        }

        #[cfg(not(unix))]
        {
            let _ = workspace;
            Err(crate::error::ToolError::Other(
                "secure workspace access is unavailable on this platform".into(),
            ))
        }
    }

    pub fn display_root(&self) -> &Path {
        &self.display_root
    }

    /// Shared access guard keyed by the opened directory's device/inode. Independently created
    /// ToolContexts and HTTP handlers for the same session workspace therefore coordinate too.
    pub async fn lock_read(&self) -> OwnedRwLockReadGuard<()> {
        self.access.clone().read_owned().await
    }

    pub async fn lock_write(&self) -> OwnedRwLockWriteGuard<()> {
        self.access.clone().write_owned().await
    }

    /// Open an existing regular file without following any symlink component.
    pub fn open_file(&self, rel: &str) -> Result<std::fs::File, crate::error::ToolError> {
        #[cfg(unix)]
        {
            let components = workspace_components(rel, false)?;
            let (parent, name) = open_parent(self.root.as_ref(), &components, false, rel)?;
            let file = open_regular_at(&parent, &name, rel)?;
            Ok(file)
        }

        #[cfg(not(unix))]
        {
            let _ = rel;
            Err(crate::error::ToolError::Other(
                "secure workspace access is unavailable on this platform".into(),
            ))
        }
    }

    /// Atomically create or replace a regular workspace file. Missing parent directories are
    /// created safely one component at a time. A symlink at the destination is replaced as a
    /// directory entry, never followed.
    pub fn atomic_write(&self, rel: &str, content: &[u8]) -> Result<(), crate::error::ToolError> {
        #[cfg(unix)]
        {
            let components = workspace_components(rel, false)?;
            let (parent, name) = open_parent(self.root.as_ref(), &components, true, rel)?;
            atomic_write_at(&parent, &name, content, rel)
        }

        #[cfg(not(unix))]
        {
            let _ = (rel, content);
            Err(crate::error::ToolError::Other(
                "secure workspace access is unavailable on this platform".into(),
            ))
        }
    }

    /// Remove an existing regular file without following the final component if it is swapped for
    /// a symlink. `unlinkat` itself only removes that symlink entry, but we reject symlinks before
    /// deletion so the API retains its "regular files only" contract.
    pub fn remove_file(&self, rel: &str) -> Result<(), crate::error::ToolError> {
        #[cfg(unix)]
        {
            let components = workspace_components(rel, false)?;
            let (parent, name) = open_parent(self.root.as_ref(), &components, false, rel)?;
            ensure_regular_entry(&parent, &name, rel)?;
            unlink_file_at(&parent, &name, rel)
        }

        #[cfg(not(unix))]
        {
            let _ = rel;
            Err(crate::error::ToolError::Other(
                "secure workspace access is unavailable on this platform".into(),
            ))
        }
    }

    /// Remove a non-directory output entry if it exists. Unlike [`Self::remove_file`], a final
    /// symlink is safely unlinked rather than rejected. This is intended for a path-based
    /// subprocess output that must be absent before launch; `unlinkat` never follows the link.
    pub fn clear_output_file(&self, rel: &str) -> Result<(), crate::error::ToolError> {
        #[cfg(unix)]
        {
            let components = workspace_components(rel, false)?;
            let (parent, name) = open_parent(self.root.as_ref(), &components, false, rel)?;
            match entry_stat(&parent, &name) {
                Ok(stat) if stat.st_mode & libc::S_IFMT == libc::S_IFDIR => {
                    Err(crate::error::ToolError::InvalidArgs(format!(
                        "{rel} is a directory, not an output file"
                    )))
                }
                Ok(_) => unlink_file_at(&parent, &name, rel),
                Err(crate::error::ToolError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }

        #[cfg(not(unix))]
        {
            let _ = rel;
            Err(crate::error::ToolError::Other(
                "secure workspace access is unavailable on this platform".into(),
            ))
        }
    }

    /// Rename a regular workspace file using held source/destination parent dirfds. Neither an
    /// intermediate component nor a replacement destination symlink is followed.
    pub fn rename_file(&self, from: &str, to: &str) -> Result<(), crate::error::ToolError> {
        #[cfg(unix)]
        {
            let from_components = workspace_components(from, false)?;
            let to_components = workspace_components(to, false)?;
            let (from_parent, from_name) =
                open_parent(self.root.as_ref(), &from_components, false, from)?;
            let (to_parent, to_name) = open_parent(self.root.as_ref(), &to_components, true, to)?;
            // Retain a stable source handle until renameat completes. A caller should also hold
            // ToolContext's write guard when coordinating with path-based processes.
            let _source = open_regular_at(&from_parent, &from_name, from)?;
            rename_file_at(&from_parent, &from_name, &to_parent, &to_name, from)
        }

        #[cfg(not(unix))]
        {
            let _ = (from, to);
            Err(crate::error::ToolError::Other(
                "secure workspace access is unavailable on this platform".into(),
            ))
        }
    }

    /// Recursively list regular files and directories below `rel`. Directory traversal is based
    /// solely on held dirfds; symlinks and non-regular special files are skipped.
    pub fn list(
        &self,
        rel: Option<&str>,
        max_depth: usize,
    ) -> Result<Vec<WorkspaceEntry>, crate::error::ToolError> {
        #[cfg(unix)]
        {
            let components = match rel {
                None => Vec::new(),
                Some(value) => workspace_components(value, true)?,
            };
            let start = open_dir_chain(self.root.as_ref(), &components, false, rel.unwrap_or(""))?;
            let mut entries = Vec::new();
            walk_directory(&start, Path::new(""), 0, max_depth, &mut entries)?;
            entries.sort_by(|left, right| left.path.cmp(&right.path));
            Ok(entries)
        }

        #[cfg(not(unix))]
        {
            let _ = (rel, max_depth);
            Err(crate::error::ToolError::Other(
                "secure workspace access is unavailable on this platform".into(),
            ))
        }
    }
}

#[cfg(unix)]
fn workspace_components(
    rel: &str,
    allow_empty: bool,
) -> Result<Vec<std::ffi::OsString>, crate::error::ToolError> {
    use std::path::Component;

    let trimmed = rel.trim();
    if trimmed.is_empty() {
        return allow_empty
            .then(Vec::new)
            .ok_or_else(|| crate::error::ToolError::PathEscape("empty path".into()));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(crate::error::ToolError::PathEscape(rel.into()));
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => components.push(name.to_os_string()),
            // `.` has identical lexical and kernel semantics and is useful for listing the root.
            Component::CurDir => {}
            // Never normalize `..`: rejecting it avoids discrepancies between validation and
            // kernel traversal and keeps every openat operation strictly beneath the root fd.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(crate::error::ToolError::PathEscape(rel.into()))
            }
        }
    }
    if components.is_empty() && !allow_empty {
        return Err(crate::error::ToolError::PathEscape(rel.into()));
    }
    Ok(components)
}

#[cfg(unix)]
fn component_cstring(
    component: &std::ffi::OsStr,
) -> Result<std::ffi::CString, crate::error::ToolError> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(component.as_bytes())
        .map_err(|_| crate::error::ToolError::InvalidArgs("path contains NUL".into()))
}

#[cfg(unix)]
fn map_workspace_io(path: &str, error: std::io::Error) -> crate::error::ToolError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return crate::error::ToolError::NotFound(path.into());
    }
    match error.raw_os_error() {
        // O_NOFOLLOW reports ELOOP for a symlink. Some kernels report ENOTDIR for an
        // intermediate symlink combined with O_DIRECTORY; fail closed in either case.
        Some(libc::ELOOP) | Some(libc::ENOTDIR) => crate::error::ToolError::PathEscape(path.into()),
        _ => crate::error::ToolError::Io(error),
    }
}

#[cfg(unix)]
fn shared_workspace_access(
    root: &std::fs::File,
) -> Result<Arc<RwLock<()>>, crate::error::ToolError> {
    use std::collections::HashMap;
    use std::os::unix::fs::MetadataExt;
    use std::sync::{Mutex as StdMutex, OnceLock, Weak};

    type Identity = (u64, u64);
    static REGISTRY: OnceLock<StdMutex<HashMap<Identity, Weak<RwLock<()>>>>> = OnceLock::new();

    let metadata = root.metadata()?;
    let identity = (metadata.dev(), metadata.ino());
    let registry = REGISTRY.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = registry.get(&identity).and_then(Weak::upgrade) {
        return Ok(existing);
    }
    // Opportunistically remove workspaces whose last capability was dropped.
    registry.retain(|_, lock| lock.strong_count() > 0);
    let access = Arc::new(RwLock::new(()));
    registry.insert(identity, Arc::downgrade(&access));
    Ok(access)
}

#[cfg(unix)]
fn clone_file(file: &std::fs::File) -> Result<std::fs::File, crate::error::ToolError> {
    file.try_clone().map_err(crate::error::ToolError::Io)
}

#[cfg(unix)]
fn open_dir_at(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path_for_error: &str,
) -> Result<std::fs::File, crate::error::ToolError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = component_cstring(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(map_workspace_io(
            path_for_error,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn mkdir_at(parent: &std::fs::File, name: &std::ffi::OsStr) -> Result<(), crate::error::ToolError> {
    use std::os::fd::AsRawFd;

    let name = component_cstring(name)?;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        // The caller always opens with O_NOFOLLOW next. An attacker racing mkdir with a symlink
        // therefore cannot redirect traversal.
        return Ok(());
    }
    Err(crate::error::ToolError::Io(error))
}

#[cfg(unix)]
fn open_dir_chain(
    root: &std::fs::File,
    components: &[std::ffi::OsString],
    create: bool,
    path_for_error: &str,
) -> Result<std::fs::File, crate::error::ToolError> {
    let mut current = clone_file(root)?;
    for component in components {
        match open_dir_at(&current, component, path_for_error) {
            Ok(next) => current = next,
            Err(crate::error::ToolError::NotFound(_)) if create => {
                mkdir_at(&current, component)?;
                current = open_dir_at(&current, component, path_for_error)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn open_parent(
    root: &std::fs::File,
    components: &[std::ffi::OsString],
    create: bool,
    path_for_error: &str,
) -> Result<(std::fs::File, std::ffi::OsString), crate::error::ToolError> {
    let (name, parents) = components
        .split_last()
        .ok_or_else(|| crate::error::ToolError::PathEscape(path_for_error.into()))?;
    let parent = open_dir_chain(root, parents, create, path_for_error)?;
    Ok((parent, name.clone()))
}

#[cfg(unix)]
fn open_regular_at(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path_for_error: &str,
) -> Result<std::fs::File, crate::error::ToolError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = component_cstring(name)?;
    // O_NONBLOCK prevents opening a hostile FIFO from blocking the async runtime before we can
    // inspect its type. It has no effect on ordinary files.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(map_workspace_io(
            path_for_error,
            std::io::Error::last_os_error(),
        ));
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(crate::error::ToolError::InvalidArgs(format!(
            "{path_for_error} is not a regular file"
        )));
    }
    Ok(file)
}

#[cfg(unix)]
fn ensure_regular_entry(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path_for_error: &str,
) -> Result<(), crate::error::ToolError> {
    // Opening and immediately dropping a handle performs an atomic no-follow type check. A later
    // swap to a symlink is still harmless because unlinkat removes the link itself, not its target.
    drop(open_regular_at(parent, name, path_for_error)?);
    Ok(())
}

#[cfg(unix)]
fn unlink_file_at(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    path_for_error: &str,
) -> Result<(), crate::error::ToolError> {
    use std::os::fd::AsRawFd;

    let name = component_cstring(name)?;
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) };
    if result < 0 {
        return Err(map_workspace_io(
            path_for_error,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn rename_file_at(
    from_parent: &std::fs::File,
    from_name: &std::ffi::OsStr,
    to_parent: &std::fs::File,
    to_name: &std::ffi::OsStr,
    path_for_error: &str,
) -> Result<(), crate::error::ToolError> {
    use std::os::fd::AsRawFd;

    let from_name = component_cstring(from_name)?;
    let to_name = component_cstring(to_name)?;
    let result = unsafe {
        libc::renameat(
            from_parent.as_raw_fd(),
            from_name.as_ptr(),
            to_parent.as_raw_fd(),
            to_name.as_ptr(),
        )
    };
    if result < 0 {
        return Err(map_workspace_io(
            path_for_error,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn atomic_write_at(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    content: &[u8],
    path_for_error: &str,
) -> Result<(), crate::error::ToolError> {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    let final_name = component_cstring(name)?;
    let mut last_collision = None;
    for _ in 0..64 {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp_name = format!(".dss-tmp-{}-{sequence}", std::process::id());
        let temp_name = std::ffi::CString::new(temp_name).expect("generated temp name has no NUL");
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                temp_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o666,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                last_collision = Some(error);
                continue;
            }
            return Err(map_workspace_io(path_for_error, error));
        }

        let mut temp = unsafe { std::fs::File::from_raw_fd(fd) };
        if let Err(error) = temp.write_all(content) {
            drop(temp);
            unsafe {
                libc::unlinkat(parent.as_raw_fd(), temp_name.as_ptr(), 0);
            }
            return Err(crate::error::ToolError::Io(error));
        }
        drop(temp);

        let renamed = unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temp_name.as_ptr(),
                parent.as_raw_fd(),
                final_name.as_ptr(),
            )
        };
        if renamed == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::unlinkat(parent.as_raw_fd(), temp_name.as_ptr(), 0);
        }
        return Err(map_workspace_io(path_for_error, error));
    }

    Err(crate::error::ToolError::Io(last_collision.unwrap_or_else(
        || {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "temporary file collision",
            )
        },
    )))
}

#[cfg(unix)]
fn directory_names(
    directory: &std::fs::File,
) -> Result<Vec<std::ffi::OsString>, crate::error::ToolError> {
    use std::ffi::CStr;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;

    // `dup` would share the directory cursor with the long-lived capability fd, making repeated
    // or concurrent scans interfere. Re-open `.` relative to the held dirfd to get an independent
    // open-file description and cursor.
    let dot = std::ffi::CString::new(".").expect("dot has no NUL");
    let independent = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            dot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if independent < 0 {
        return Err(crate::error::ToolError::Io(std::io::Error::last_os_error()));
    }
    let stream = unsafe { libc::fdopendir(independent) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(independent);
        }
        return Err(crate::error::ToolError::Io(error));
    }

    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            break;
        }
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        names.push(std::ffi::OsString::from_vec(bytes.to_vec()));
    }
    names.sort();
    Ok(names)
}

#[cfg(unix)]
fn entry_stat(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
) -> Result<libc::stat, crate::error::ToolError> {
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;

    let name = component_cstring(name)?;
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        return Err(crate::error::ToolError::Io(std::io::Error::last_os_error()));
    }
    Ok(unsafe { stat.assume_init() })
}

#[cfg(unix)]
fn walk_directory(
    directory: &std::fs::File,
    prefix: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<WorkspaceEntry>,
) -> Result<(), crate::error::ToolError> {
    const SKIP: &[&str] = &[".git", ".venv", "__pycache__", "node_modules", "target"];

    for name in directory_names(directory)? {
        let display_name = name.to_string_lossy().into_owned();
        if SKIP.contains(&display_name.as_str())
            || display_name.starts_with(".dss-sandbox-")
            || display_name.starts_with(".dss-tmp-")
        {
            continue;
        }
        let stat = match entry_stat(directory, &name) {
            Ok(stat) => stat,
            // Concurrent deletion is normal during a scan. Other failures remain visible.
            Err(crate::error::ToolError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        let kind = stat.st_mode & libc::S_IFMT;
        let relative = prefix.join(&name);
        let relative_display = relative.to_string_lossy().replace('\\', "/");
        if kind == libc::S_IFDIR {
            out.push(WorkspaceEntry {
                path: relative_display,
                name: display_name,
                size: 0,
                is_dir: true,
            });
            if depth < max_depth {
                // fstatat and openat are intentionally separate. If the entry is replaced in
                // between, O_NOFOLLOW prevents a replacement symlink from being traversed.
                match open_dir_at(directory, &name, &relative.to_string_lossy()) {
                    Ok(child) => walk_directory(&child, &relative, depth + 1, max_depth, out)?,
                    Err(crate::error::ToolError::NotFound(_))
                    | Err(crate::error::ToolError::PathEscape(_)) => continue,
                    Err(error) => return Err(error),
                }
            }
        } else if kind == libc::S_IFREG {
            out.push(WorkspaceEntry {
                path: relative_display,
                name: display_name,
                size: stat.st_size.max(0) as u64,
                is_dir: false,
            });
        }
        // Symlinks, sockets, devices and FIFOs are deliberately omitted.
    }
    Ok(())
}

/// 词法规范化：消解 `..` / `.`，不访问文件系统。
fn lexically_resolve(p: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        use std::path::Component;
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::ToolContext;
    use crate::error::ToolError;

    fn test_dir(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("dss-tools-{label}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn resolves_new_file_inside_workspace() {
        let root = test_dir("inside");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = ToolContext::new(root.clone());
        let resolved = ctx.resolve_in_workspace("notes/result.md").unwrap();
        assert!(resolved.starts_with(root.canonicalize().unwrap()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_parent_escape() {
        let root = test_dir("parent");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = ToolContext::new(root.clone());
        assert!(ctx.resolve_in_workspace("../outside.txt").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_new_file_beneath_symlinked_parent() {
        let root = test_dir("symlink-root");
        let outside = test_dir("symlink-outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("redirect")).unwrap();
        let ctx = ToolContext::new(root.clone());
        assert!(ctx.resolve_in_workspace("redirect/new.txt").is_err());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn secure_write_never_follows_parent_or_destination_symlinks() {
        let root = test_dir("secure-write-root");
        let outside = test_dir("secure-write-outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("redirect")).unwrap();
        std::os::unix::fs::symlink(outside.join("keep.txt"), root.join("replace.txt")).unwrap();
        std::os::unix::fs::symlink(outside.join("keep.txt"), root.join("generated.pdf")).unwrap();
        std::os::unix::fs::symlink(outside.join("keep.txt"), root.join("final.pdf")).unwrap();
        std::fs::write(root.join("staged.pdf"), "pdf-bytes").unwrap();
        let workspace = super::SecureWorkspace::open(&root).unwrap();

        assert!(matches!(
            workspace.atomic_write("redirect/new.txt", b"blocked"),
            Err(ToolError::PathEscape(_))
        ));
        workspace
            .atomic_write("replace.txt", b"inside")
            .expect("rename replaces the symlink entry without following it");
        workspace
            .clear_output_file("generated.pdf")
            .expect("clear output unlinks the symlink itself");
        workspace
            .rename_file("staged.pdf", "final.pdf")
            .expect("rename replaces destination symlink without following it");

        assert!(!outside.join("new.txt").exists());
        assert!(!root.join("generated.pdf").exists());
        assert_eq!(
            std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
            "outside"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("replace.txt")).unwrap(),
            "inside"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("final.pdf")).unwrap(),
            "pdf-bytes"
        );

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn held_parent_dirfd_is_not_redirected_by_replacement_symlink() {
        let root = test_dir("dirfd-race-root");
        let outside = test_dir("dirfd-race-outside");
        std::fs::create_dir_all(root.join("slot")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let workspace = super::SecureWorkspace::open(&root).unwrap();
        let components = super::workspace_components("slot/result.txt", false).unwrap();
        let (held_parent, name) = super::open_parent(
            workspace.root.as_ref(),
            &components,
            false,
            "slot/result.txt",
        )
        .unwrap();

        // Replace the pathname after validation/opening but before the write. Path-based code
        // would now write through `slot` into outside; renameat on held_parent must not.
        std::fs::rename(root.join("slot"), root.join("original-slot")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("slot")).unwrap();
        super::atomic_write_at(&held_parent, &name, b"safe", "slot/result.txt").unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("original-slot/result.txt")).unwrap(),
            "safe"
        );
        assert!(!outside.join("result.txt").exists());
        assert!(matches!(
            workspace.atomic_write("slot/second.txt", b"blocked"),
            Err(ToolError::PathEscape(_))
        ));

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn secure_listing_is_repeatable_and_skips_symlink_subtrees() {
        let root = test_dir("secure-list-root");
        let outside = test_dir("secure-list-outside");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("notes/inside.txt"), "inside").unwrap();
        std::fs::write(outside.join("secret.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("redirect")).unwrap();
        let workspace = super::SecureWorkspace::open(&root).unwrap();

        let first = workspace.list(None, 3).unwrap();
        let second = workspace.list(None, 3).unwrap();
        assert_eq!(first, second);
        assert!(first.iter().any(|entry| entry.path == "notes/inside.txt"));
        assert!(!first.iter().any(|entry| entry.path.contains("secret")));
        assert!(!first.iter().any(|entry| entry.path == "redirect"));

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[tokio::test]
    async fn workspace_write_guard_excludes_all_other_operations() {
        let root = test_dir("workspace-lock");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = ToolContext::new(root.clone());
        let independently_created_ctx = ToolContext::new(root.clone());

        let write_guard = ctx.lock_workspace_write().await;
        assert!(ctx.workspace_access.clone().try_read_owned().is_err());
        assert!(ctx.workspace_access.clone().try_write_owned().is_err());
        assert!(independently_created_ctx
            .workspace_access
            .clone()
            .try_write_owned()
            .is_err());
        drop(write_guard);
        let shared_process_guard = ctx
            .workspace_access
            .clone()
            .try_read_owned()
            .expect("first shared process guard");
        assert!(independently_created_ctx
            .workspace_access
            .clone()
            .try_read_owned()
            .is_ok());
        assert!(ctx.workspace_access.clone().try_write_owned().is_err());
        drop(shared_process_guard);
        assert!(ctx.workspace_access.clone().try_write_owned().is_ok());

        std::fs::remove_dir_all(root).unwrap();
    }
}
