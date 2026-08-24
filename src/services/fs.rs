use crate::error::{AppError, AppResult};
use camino::Utf8PathBuf;
use futures::channel::oneshot;
use gpui::{App, PathPromptOptions};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::{Builder, NamedTempFile};

/// Opens a native picker that accepts either one file or one directory.
pub fn pick_open_path_async(cx: &App) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
    cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: true,
        multiple: false,
        prompt: Some("Open".into()),
    })
}

/// Returns true if the path has a supported markdown extension.
pub fn is_markdown_path(path: &Utf8PathBuf) -> bool {
    path.extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("md")
            || ext.eq_ignore_ascii_case("markdown")
            || ext.eq_ignore_ascii_case("mdown")
    })
}

/// Async version of pick_save_path using GPUI's native dialog
/// Returns a receiver that will contain the selected path
pub fn pick_save_path_async(
    cx: &App,
    default: Option<&Utf8PathBuf>,
) -> oneshot::Receiver<Result<Option<PathBuf>>> {
    let home_dir = directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let (directory, suggested_name) = if let Some(path) = default {
        let dir = path
            .parent()
            .map(|p| p.as_std_path().to_path_buf())
            .unwrap_or_else(|| home_dir.clone());
        let name = path.file_name().unwrap_or("untitled.md");
        (dir, Some(name))
    } else {
        (home_dir, Some("untitled.md"))
    };

    cx.prompt_for_new_path(&directory, suggested_name)
}

pub fn read_to_string(path: &Utf8PathBuf) -> AppResult<String> {
    Ok(fs::read_to_string(path)?)
}

/// Atomically replaces a file after fully syncing its contents and preserving existing permissions.
pub fn write_atomic(path: &Path, contents: &[u8]) -> AppResult<()> {
    write_atomic_with(path, |file| file.write_all(contents))
}

/// Performs the atomic replacement while allowing focused tests to inject write failures.
fn write_atomic_with(
    path: &Path,
    write_contents: impl FnOnce(&mut File) -> io::Result<()>,
) -> AppResult<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let existing_permissions = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(AppError::file_operation(
                "read permissions for",
                path,
                error,
            ));
        }
    };
    let mut tmp = create_atomic_temp(parent)
        .map_err(|error| AppError::file_operation("create temporary file for", path, error))?;

    write_contents(tmp.as_file_mut())
        .map_err(|error| AppError::file_operation("write", path, error))?;
    tmp.as_file_mut()
        .flush()
        .map_err(|error| AppError::file_operation("flush", path, error))?;

    if let Some(permissions) = existing_permissions {
        tmp.as_file_mut()
            .set_permissions(permissions)
            .map_err(|error| AppError::file_operation("preserve permissions for", path, error))?;
    }

    tmp.as_file()
        .sync_all()
        .map_err(|error| AppError::file_operation("sync", path, error))?;
    tmp.persist(path)
        .map_err(|error| AppError::file_operation("replace", path, error.error))?;
    sync_parent_directory(parent, path)?;
    Ok(())
}

/// Creates a same-directory temporary file with ordinary new-file permissions.
fn create_atomic_temp(parent: &Path) -> io::Result<NamedTempFile> {
    let mut builder = Builder::new();
    builder.prefix(".aster-save-");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o666));
    }

    builder.tempfile_in(parent)
}

/// Syncs the directory entry created by the atomic rename on Unix platforms.
#[cfg(unix)]
fn sync_parent_directory(parent: &Path, target: &Path) -> AppResult<()> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| AppError::file_operation("sync parent directory for", target, error))
}

/// Directory syncing is not required by Aster's non-Unix targets.
#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path, _target: &Path) -> AppResult<()> {
    Ok(())
}

// Re-export Result for use with oneshot receivers
pub use anyhow::Result;

#[cfg(test)]
mod tests {
    use super::{is_markdown_path, write_atomic, write_atomic_with};
    use camino::Utf8PathBuf;
    use std::fs;
    use std::io::{self, Write};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn detects_supported_markdown_extensions() {
        assert!(is_markdown_path(&Utf8PathBuf::from("/tmp/doc.md")));
        assert!(is_markdown_path(&Utf8PathBuf::from("/tmp/doc.MD")));
        assert!(is_markdown_path(&Utf8PathBuf::from("/tmp/doc.markdown")));
        assert!(is_markdown_path(&Utf8PathBuf::from("/tmp/doc.mdown")));
    }

    #[test]
    fn rejects_non_markdown_extensions() {
        assert!(!is_markdown_path(&Utf8PathBuf::from("/tmp/doc.txt")));
        assert!(!is_markdown_path(&Utf8PathBuf::from("/tmp/doc")));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_existing_file_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.md");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        write_atomic(&path, b"new").unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn atomic_write_replaces_contents_without_leaving_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.md");

        write_atomic(&path, b"complete").unwrap();

        let entries = fs::read_dir(directory.path()).unwrap().count();
        assert_eq!(
            (fs::read_to_string(path).unwrap(), entries),
            ("complete".into(), 1)
        );
    }

    #[test]
    fn atomic_write_failure_preserves_the_previous_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.md");
        fs::write(&path, "original").unwrap();

        let result = write_atomic_with(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected failure"))
        });

        assert_eq!(
            (result.is_err(), fs::read_to_string(path).unwrap()),
            (true, "original".into())
        );
    }
}
