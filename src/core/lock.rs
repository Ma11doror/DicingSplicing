//! Which projects the editor has open. It holds a lock on `editor.lock` in the folder of the
//! project it shows, and ds-tool leaves such a project alone: the editor saves what it shows by
//! itself, and would write over what ds-tool changed. The system lets the lock go when the editor
//! closes the project, and also when it ends in any other way, a crash too.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

pub const LOCK_FILE: &str = "editor.lock";

/// The editor's hold on the project it has open. Dropping it lets the project go.
pub struct EditorLock {
    file: Option<File>,
    path: PathBuf,
}

impl EditorLock {
    /// Holds the project in `dir`. None if it can't, as in a folder that can't be written to: the
    /// editor works on without it.
    pub fn take(dir: &Path) -> Option<Self> {
        let path = dir.join(LOCK_FILE);
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).ok()?;
        // Shared: other programs can still read the file, and so copy the folder.
        file.try_lock_shared().ok()?;
        Some(Self { file: Some(file), path })
    }
}

impl Drop for EditorLock {
    fn drop(&mut self) {
        // Closing the file lets the lock go; then the file itself goes.
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

/// Whether an editor has the project in `dir` open.
pub fn held(dir: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(dir.join(LOCK_FILE)) else {
        return false;
    };
    matches!(file.try_lock(), Err(TryLockError::WouldBlock))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_is_held_while_the_lock_lives() {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_lock_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(!held(&dir));
        let lock = EditorLock::take(&dir).unwrap();
        assert!(held(&dir));
        drop(lock);
        assert!(!held(&dir));
        assert!(!dir.join(LOCK_FILE).exists(), "the file goes with the lock");
        // A file left behind holds nothing: the lock was the system's, and went with its program.
        fs::write(dir.join(LOCK_FILE), "").unwrap();
        assert!(!held(&dir));
        let _ = fs::remove_dir_all(&dir);
    }
}
