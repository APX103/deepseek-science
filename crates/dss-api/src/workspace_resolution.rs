//! Safe resolution for session workspaces after a data-directory relocation.

use std::path::{Component, Path, PathBuf};

use dss_db::repo::SessionRow;

use crate::db as dbq;
use crate::state::AppState;

/// Resolve the workspace recorded for a session.
///
/// Existing persisted paths are authoritative. When that path is missing, the only recovery
/// location considered is `<current data_dir>/workspaces/<session id>`, and only a real directory
/// at that exact path is accepted. A successful recovery is persisted with compare-and-swap so
/// concurrent requests cannot clobber a newer path.
pub(crate) async fn resolve_session_workspace(
    state: &AppState,
    row: &SessionRow,
) -> Result<PathBuf, dss_db::DbError> {
    let persisted = PathBuf::from(&row.workspace);
    if path_exists(&persisted)? {
        return Ok(persisted);
    }

    let fallback = fallback_workspace(&state.settings.data_dir, &row.id).ok_or_else(|| {
        dss_db::DbError::Other(format!(
            "workspace unavailable for session {}: persisted path is missing and session id is not a safe path component",
            row.id
        ))
    })?;
    if !is_real_directory(&fallback)? {
        return Err(dss_db::DbError::Other(format!(
            "workspace unavailable for session {}: persisted path is missing and no workspace exists in the current data directory",
            row.id
        )));
    }

    let fallback_text = fallback.to_str().ok_or_else(|| {
        dss_db::DbError::Other(format!(
            "workspace unavailable for session {}: current workspace path is not valid UTF-8",
            row.id
        ))
    })?;
    let rebased = dbq::rebase_session_workspace(
        &state.db,
        row.id.clone(),
        row.workspace.clone(),
        fallback_text.to_owned(),
    )
    .await?;
    if rebased {
        tracing::warn!(
            session_id = %row.id,
            old_workspace = %row.workspace,
            new_workspace = %fallback.display(),
            "rebased missing session workspace into the current data directory"
        );
        return Ok(fallback);
    }

    // Another request changed the row after our read. Honor that newer path if it is now valid;
    // never overwrite it or search elsewhere.
    let latest = dbq::get_session_row(&state.db, row.id.clone())
        .await?
        .ok_or_else(|| dss_db::DbError::NotFound(format!("session {}", row.id)))?;
    let latest_path = PathBuf::from(&latest.workspace);
    if path_exists(&latest_path)? {
        Ok(latest_path)
    } else {
        Err(dss_db::DbError::Conflict(format!(
            "session {} workspace changed while it was being restored",
            row.id
        )))
    }
}

fn path_exists(path: &Path) -> Result<bool, dss_db::DbError> {
    path.try_exists().map_err(|error| {
        dss_db::DbError::Other(format!(
            "could not inspect workspace {}: {error}",
            path.display()
        ))
    })
}

fn is_real_directory(path: &Path) -> Result<bool, dss_db::DbError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(dss_db::DbError::Other(format!(
            "could not inspect fallback workspace {}: {error}",
            path.display()
        ))),
    }
}

fn fallback_workspace(data_dir: &Path, sid: &str) -> Option<PathBuf> {
    let mut components = Path::new(sid).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if !component.is_empty() => {
            Some(data_dir.join("workspaces").join(component))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use dss_core::settings::ServerSettings;
    use dss_core::{LlmEnvOverrides, LlmSettings, Settings};

    use super::{fallback_workspace, resolve_session_workspace};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "deepseek-science-workspace-resolution-{label}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn build_test_state(test_dir: &TestDir) -> crate::state::AppState {
        crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state")
    }

    #[test]
    fn fallback_accepts_only_one_normal_session_id_component() {
        let root = Path::new("/current-data");
        assert_eq!(
            fallback_workspace(root, "abc123"),
            Some(root.join("workspaces/abc123"))
        );
        for unsafe_id in ["", ".", "..", "../escape", "nested/session", "/absolute"] {
            assert_eq!(fallback_workspace(root, unsafe_id), None, "{unsafe_id}");
        }
    }

    #[tokio::test]
    async fn existing_persisted_workspace_remains_authoritative() {
        let test_dir = TestDir::new("preserve-existing");
        let state = build_test_state(&test_dir).await;
        let sid = "preserve-existing";
        let persisted = test_dir.path().join("external-workspace");
        let fallback = test_dir.path().join("workspaces").join(sid);
        std::fs::create_dir_all(&persisted).unwrap();
        std::fs::create_dir_all(&fallback).unwrap();
        let row = crate::db::create_session_row(
            &state.db,
            sid.into(),
            persisted.to_string_lossy().into_owned(),
            None,
            Some(dss_db::DEFAULT_PROJECT_ID.into()),
        )
        .await
        .unwrap();

        let resolved = resolve_session_workspace(&state, &row).await.unwrap();
        assert_eq!(resolved, persisted);
        let stored = crate::db::get_session_row(&state.db, sid.into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.workspace, persisted.to_string_lossy());
    }

    #[tokio::test]
    async fn missing_persisted_workspace_rebases_to_exact_current_location() {
        let test_dir = TestDir::new("rebase-missing");
        let state = build_test_state(&test_dir).await;
        let sid = "rebase-missing";
        let old_workspace = test_dir.path().join("old-root/workspaces").join(sid);
        let fallback = test_dir.path().join("workspaces").join(sid);
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(fallback.join("restored.md"), "restored").unwrap();
        let row = crate::db::create_session_row(
            &state.db,
            sid.into(),
            old_workspace.to_string_lossy().into_owned(),
            None,
            Some(dss_db::DEFAULT_PROJECT_ID.into()),
        )
        .await
        .unwrap();
        let original_updated_at = row.updated_at.clone();

        let resolved = resolve_session_workspace(&state, &row).await.unwrap();
        assert_eq!(resolved, fallback);
        assert_eq!(
            std::fs::read_to_string(resolved.join("restored.md")).unwrap(),
            "restored"
        );
        let stored = crate::db::get_session_row(&state.db, sid.into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.workspace, fallback.to_string_lossy());
        assert_eq!(stored.updated_at, original_updated_at);
    }

    #[tokio::test]
    async fn missing_fallback_does_not_rewrite_the_database() {
        let test_dir = TestDir::new("no-fallback");
        let state = build_test_state(&test_dir).await;
        let sid = "no-fallback";
        let old_workspace = test_dir.path().join("old-root/workspaces").join(sid);
        let row = crate::db::create_session_row(
            &state.db,
            sid.into(),
            old_workspace.to_string_lossy().into_owned(),
            None,
            Some(dss_db::DEFAULT_PROJECT_ID.into()),
        )
        .await
        .unwrap();

        let error = resolve_session_workspace(&state, &row)
            .await
            .expect_err("missing exact fallback must fail closed");
        assert!(error.to_string().contains("no workspace exists"));
        let stored = crate::db::get_session_row(&state.db, sid.into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.workspace, row.workspace);
    }
}
