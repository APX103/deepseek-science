//! Workspace file endpoints with dirfd-based path confinement.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, Response, StatusCode};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use dss_tools::{SecureWorkspace, ToolError};

use crate::db as dbq;
use crate::state::AppState;
use crate::workspace_resolution::resolve_session_workspace;

fn json_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

async fn workspace_for(
    state: &AppState,
    sid: String,
) -> Result<SecureWorkspace, (StatusCode, Json<Value>)> {
    let row = dbq::get_session_row(&state.db, sid)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| json_error(StatusCode::NOT_FOUND, "session not found"))?;
    let workspace = resolve_session_workspace(state, &row)
        .await
        .map_err(|error| match error {
            dss_db::DbError::NotFound(message) => json_error(StatusCode::NOT_FOUND, &message),
            dss_db::DbError::Conflict(message) => json_error(StatusCode::CONFLICT, &message),
            error => json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        })?;
    SecureWorkspace::open(&workspace).map_err(|error| {
        let (status, _) = map_workspace_error(&error);
        json_error(status, &format!("workspace unavailable: {error}"))
    })
}

fn map_workspace_error(error: &ToolError) -> (StatusCode, Json<Value>) {
    let status = match error {
        ToolError::PathEscape(_) => StatusCode::FORBIDDEN,
        ToolError::InvalidArgs(_) => StatusCode::BAD_REQUEST,
        ToolError::NotFound(_) => StatusCode::NOT_FOUND,
        ToolError::Io(error) => match error.kind() {
            std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
            std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
            std::io::ErrorKind::IsADirectory | std::io::ErrorKind::NotADirectory => {
                StatusCode::BAD_REQUEST
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        },
        ToolError::Other(_) | ToolError::Timeout(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    json_error(status, &error.to_string())
}

#[derive(Serialize)]
pub struct WorkspaceFile {
    path: String,
    size: u64,
    name: String,
}

#[derive(Serialize)]
pub struct ListFilesResponse {
    files: Vec<WorkspaceFile>,
}

/// `GET /api/sessions/{sid}/files`.
pub async fn list_files(
    State(state): State<AppState>,
    Path(sid): Path<String>,
) -> Result<Json<ListFilesResponse>, (StatusCode, Json<Value>)> {
    let workspace = workspace_for(&state, sid).await?;
    let files = tokio::task::spawn_blocking(move || workspace.list(None, 8))
        .await
        .map_err(|e| {
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("file scan failed: {e}"),
            )
        })?
        .map_err(|error| map_workspace_error(&error))?
        .into_iter()
        .filter(|entry| !entry.is_dir)
        .map(|entry| WorkspaceFile {
            path: entry.path,
            size: entry.size,
            name: entry.name,
        })
        .collect();
    Ok(Json(ListFilesResponse { files }))
}

/// `GET /api/sessions/{sid}/files/{*path}`.
pub async fn read_file(
    State(state): State<AppState>,
    Path((sid, path)): Path<(String, String)>,
) -> Result<Response<Body>, (StatusCode, Json<Value>)> {
    let workspace = workspace_for(&state, sid).await?;
    let requested_path = path.clone();
    let file = tokio::task::spawn_blocking(move || workspace.open_file(&requested_path))
        .await
        .map_err(|error| {
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("file open task failed: {error}"),
            )
        })?
        .map_err(|error| map_workspace_error(&error))?;
    // Metadata and stream are derived from the same already-open handle. A replacement symlink at
    // the original pathname cannot change either the bytes or the Content-Length we return.
    let metadata = file
        .metadata()
        .map_err(|error| json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    let handle = tokio::fs::File::from_std(file);
    let content_type = match std::path::Path::new(&path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "json" => "application/json; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "md" | "tex" | "txt" | "py" | "rs" | "toml" | "yaml" | "yml" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, metadata.len())
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(handle)))
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))
}

/// `DELETE /api/sessions/{sid}/files/{*path}`.
pub async fn delete_file(
    State(state): State<AppState>,
    Path((sid, path)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let workspace = workspace_for(&state, sid).await?;
    let _workspace_guard = workspace.lock_write().await;
    tokio::task::spawn_blocking(move || workspace.remove_file(&path))
        .await
        .map_err(|error| {
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("file deletion task failed: {error}"),
            )
        })?
        .map_err(|error| map_workspace_error(&error))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use axum::extract::{Path as AxumPath, State};
    use dss_core::settings::ServerSettings;
    use dss_core::{LlmEnvOverrides, LlmSettings, Settings};
    use dss_tools::{SecureWorkspace, ToolError};

    use super::list_files;

    #[test]
    fn existing_file_is_confined_to_workspace() {
        let root = std::env::temp_dir().join(format!("dss-api-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("paper.md"), "ok").unwrap();
        let workspace = SecureWorkspace::open(&root).unwrap();
        assert!(workspace.open_file("paper.md").is_ok());
        assert!(matches!(
            workspace.open_file("../outside.md"),
            Err(ToolError::PathEscape(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn file_listing_recovers_an_exact_workspace_after_data_directory_move() {
        let root = std::env::temp_dir().join(format!(
            "dss-api-relocated-files-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state = crate::state::build_state(Settings {
            data_dir: root.clone(),
            data_dir_is_default: false,
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
        })
        .await
        .expect("build test application state");
        let sid = "relocated-files";
        let workspace = root.join("workspaces").join(sid);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("paper.md"), "restored").unwrap();
        let obsolete = root.join("old-data/workspaces").join(sid);
        crate::db::create_session_row(
            &state.db,
            sid.into(),
            obsolete.to_string_lossy().into_owned(),
            None,
            Some(dss_db::DEFAULT_PROJECT_ID.into()),
        )
        .await
        .unwrap();

        let response = list_files(State(state.clone()), AxumPath(sid.into()))
            .await
            .expect("list files from relocated workspace")
            .0;
        assert_eq!(response.files.len(), 1);
        assert_eq!(response.files[0].path, "paper.md");
        let stored = crate::db::get_session_row(&state.db, sid.into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.workspace, workspace.to_string_lossy());

        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn api_file_handle_does_not_follow_replacement_symlink() {
        let root =
            std::env::temp_dir().join(format!("dss-api-files-race-root-{}", std::process::id()));
        let outside =
            std::env::temp_dir().join(format!("dss-api-files-race-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("paper.md"), "inside").unwrap();
        std::fs::write(outside.join("secret.md"), "outside-secret").unwrap();

        let workspace = SecureWorkspace::open(&root).unwrap();
        let mut handle = workspace.open_file("paper.md").unwrap();
        std::fs::remove_file(root.join("paper.md")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.md"), root.join("paper.md")).unwrap();

        let mut content = String::new();
        handle.read_to_string(&mut content).unwrap();
        assert_eq!(content, "inside");
        assert!(matches!(
            workspace.open_file("paper.md"),
            Err(ToolError::PathEscape(_))
        ));

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn api_delete_rejects_symlink_and_preserves_outside_target() {
        let root = std::env::temp_dir().join(format!("dss-api-delete-root-{}", std::process::id()));
        let outside =
            std::env::temp_dir().join(format!("dss-api-delete-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.md"), "keep").unwrap();
        std::os::unix::fs::symlink(outside.join("keep.md"), root.join("paper.md")).unwrap();

        let workspace = SecureWorkspace::open(&root).unwrap();
        assert!(matches!(
            workspace.remove_file("paper.md"),
            Err(ToolError::PathEscape(_))
        ));
        assert_eq!(
            std::fs::read_to_string(outside.join("keep.md")).unwrap(),
            "keep"
        );

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
