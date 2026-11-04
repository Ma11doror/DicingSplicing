//! Writing the files the editor keeps.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

/// Replaces the file in one step: a crash leaves either the old contents or the new ones.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    };
    write()
        .inspect_err(|_| {
            let _ = fs::remove_file(&temp);
        })
        .with_context(|| format!("can't write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_nothing_else() {
        let dir = std::env::temp_dir().join(format!("simple_editor_files_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.json");

        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);

        assert!(write_atomic(&dir.join("no_such_folder").join("data.json"), b"x").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
