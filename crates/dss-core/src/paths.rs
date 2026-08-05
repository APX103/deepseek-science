//! data_dir 解析与 SSD 软链逻辑。

use std::path::{Path, PathBuf};

use crate::error::Error;

/// 默认数据目录名（位于 home 目录下）。
pub const DEFAULT_DATA_DIR_NAME: &str = ".deepseek-science";

/// 开发机 SSD 数据目录；若存在且默认 data_dir 可替换，则软链过去。
pub const SSD_DATA_DIR: &str = "/Volumes/ssd/main_link/.deepseek-science";

/// data_dir 下启动时确保存在的子目录。
pub const DATA_SUBDIRS: [&str; 3] = ["workspaces", "logs", "skills"];

/// 解析 data_dir。
///
/// 返回 `(data_dir, is_default)`：`is_default` 为 true 表示未设置 `DSS_DATA_DIR`，
/// 此时允许尝试 SSD 软链。
pub fn resolve_data_dir() -> Result<(PathBuf, bool), Error> {
    if let Some(dir) = std::env::var_os("DSS_DATA_DIR") {
        return Ok((PathBuf::from(dir), false));
    }
    let home = std::env::var_os("HOME").ok_or(Error::NoHome)?;
    Ok((PathBuf::from(home).join(DEFAULT_DATA_DIR_NAME), true))
}

/// 确保 data_dir 及其子目录存在；`allow_ssd_symlink` 时按约定尝试 SSD 软链。
pub fn ensure_data_dir(data_dir: &Path, allow_ssd_symlink: bool) -> Result<(), Error> {
    if allow_ssd_symlink {
        maybe_link_ssd(data_dir)?;
    }
    std::fs::create_dir_all(data_dir)?;
    set_private_directory_permissions(data_dir)?;
    for sub in DATA_SUBDIRS {
        let path = data_dir.join(sub);
        std::fs::create_dir_all(&path)?;
        set_private_directory_permissions(&path)?;
    }
    Ok(())
}

fn set_private_directory_permissions(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Research transcripts, generated files, logs, the SQLite database,
        // and provider credentials all live below this tree.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// SSD 软链：若 [`SSD_DATA_DIR`] 存在且 `data_dir` 不是软链，则把 `data_dir` 软链过去。
///
/// 仅当 `data_dir` 不存在或为空目录时才操作，避免破坏已有数据；其他情况跳过并告警。
/// 操作前打印将做什么。
fn maybe_link_ssd(data_dir: &Path) -> Result<(), Error> {
    let ssd = Path::new(SSD_DATA_DIR);
    if !ssd.is_dir() {
        return Ok(());
    }

    match std::fs::symlink_metadata(data_dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                tracing::debug!(path = %data_dir.display(), "data_dir is already a symlink, leaving as-is");
                return Ok(());
            }
            if meta.is_dir() {
                if std::fs::read_dir(data_dir)?.next().is_some() {
                    tracing::warn!(
                        path = %data_dir.display(),
                        ssd = %ssd.display(),
                        "data_dir exists and is not empty; skipping SSD symlink to protect existing data"
                    );
                    return Ok(());
                }
                tracing::info!(
                    path = %data_dir.display(),
                    ssd = %ssd.display(),
                    "data_dir is an empty directory; will remove it and create symlink to SSD data dir"
                );
                std::fs::remove_dir(data_dir)?;
            } else {
                tracing::warn!(
                    path = %data_dir.display(),
                    "data_dir exists and is not a directory; skipping SSD symlink"
                );
                return Ok(());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(
                path = %data_dir.display(),
                ssd = %ssd.display(),
                "data_dir does not exist; will create symlink to SSD data dir"
            );
        }
        Err(e) => return Err(e.into()),
    }

    std::os::unix::fs::symlink(ssd, data_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn data_tree_is_private_even_with_a_permissive_process_umask() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "deepseek-science-private-data-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        ensure_data_dir(&root, false).expect("create private data tree");
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for subdir in DATA_SUBDIRS {
            assert_eq!(
                std::fs::metadata(root.join(subdir))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
