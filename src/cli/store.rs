//! Reading and writing projects for ds-tool. A project the editor has open is left alone. Before a
//! change, what was there is kept in the project's folder, so that `undo` can bring it back.

use crate::core::files;
use crate::core::lock;
use crate::core::project::{Project, PROJECT_FILE};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The last change ds-tool made to the project in the same folder.
pub const UNDO_FILE: &str = "ds-tool-undo.json";

/// A change: the command, and the project file before and after it.
#[derive(Serialize, Deserialize)]
struct Change {
    command: String,
    before: String,
    after: String,
}

/// The folder of the project `place` names: the folder itself, or its project.json.
pub fn folder_of(place: &str) -> PathBuf {
    let path = std::path::absolute(place).unwrap_or_else(|_| PathBuf::from(place));
    match path.file_name() {
        Some(name) if name == PROJECT_FILE => path.parent().map_or(path.clone(), Path::to_owned),
        _ => path,
    }
}

/// The project in `dir`.
pub fn load(dir: &Path) -> Result<Project> {
    if !dir.join(PROJECT_FILE).is_file() {
        bail!("there is no project in {}: it has no {PROJECT_FILE}", dir.display());
    }
    Project::load(dir)
}

/// An error if the editor has the project in `dir` open.
pub fn refuse_if_open(dir: &Path) -> Result<()> {
    if lock::held(dir) {
        bail!("the project is open in the editor. Close it there first (Projects, at the top left), and run this again: the editor saves what it shows by itself, and would write over the change");
    }
    Ok(())
}

/// Saves a new project into `dir`, making the folder if it is not there. A folder with a project
/// in it already is left as it is.
pub fn create(dir: &Path, project: &Project) -> Result<()> {
    if dir.join(PROJECT_FILE).exists() {
        bail!("there is a project in {} already", dir.display());
    }
    fs::create_dir_all(dir).with_context(|| format!("can't make the folder {}", dir.display()))?;
    // A change from before has nothing to do with the new project.
    let _ = fs::remove_file(dir.join(UNDO_FILE));
    project.save(dir)
}

/// Changes the project in `dir` with `change`, which says what it did, and saves it, keeping what
/// it was for `undo`. When `change` fails, nothing is written. `command` names the change.
pub fn change(dir: &Path, command: &str, change: impl FnOnce(&mut Project) -> Result<String>) -> Result<(String, Project)> {
    refuse_if_open(dir)?;
    let mut project = load(dir)?;
    let file = dir.join(PROJECT_FILE);
    let before = fs::read_to_string(&file).with_context(|| format!("can't read {}", file.display()))?;
    let done = change(&mut project)?;
    project.save(dir)?;
    let after = fs::read_to_string(&file).with_context(|| format!("can't read {}", file.display()))?;
    let change = Change { command: command.to_owned(), before, after };
    files::write_atomic(&dir.join(UNDO_FILE), &serde_json::to_vec_pretty(&change)?)?;
    Ok((done, project))
}

/// Brings the project in `dir` back to what it was before the last change ds-tool made to it, if
/// nothing has changed it since. Says which change it took back.
pub fn undo(dir: &Path) -> Result<(String, Project)> {
    refuse_if_open(dir)?;
    let record = dir.join(UNDO_FILE);
    let Ok(text) = fs::read_to_string(&record) else {
        bail!("there is nothing to undo: ds-tool has not changed the project since it was made or last undone");
    };
    let change: Change = serde_json::from_str(&text).with_context(|| format!("{} is damaged", record.display()))?;
    let now = fs::read_to_string(dir.join(PROJECT_FILE)).with_context(|| format!("can't read the project in {}", dir.display()))?;
    if now != change.after {
        bail!("the project has changed since `{}`, in the editor or otherwise: undo would lose that, so it does nothing", change.command);
    }
    files::write_atomic(&dir.join(PROJECT_FILE), change.before.as_bytes())?;
    fs::remove_file(&record).with_context(|| format!("can't remove {}", record.display()))?;
    Ok((format!("Took back `{}`.", change.command), load(dir)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::pieces::tests::{laid_out, piece, video};
    use crate::cli::pieces::{new_project, put};
    use crate::core::lock::EditorLock;
    use crate::core::preset::PresetId;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_store_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn append(dir: &Path, file: &str) -> Result<(String, Project)> {
        change(dir, &format!("append {file}"), |project| {
            put(project, &piece(file, None, None, None), &video(10.0), None)?;
            Ok(format!("Put {file}."))
        })
    }

    #[test]
    fn a_change_is_saved_and_undo_takes_it_back() {
        let dir = temp_dir("undo");
        create(&dir, &new_project("Lesson", PresetId::Landscape)).unwrap();
        assert!(create(&dir, &new_project("Again", PresetId::Landscape)).unwrap_err().to_string().starts_with("there is a project in"));
        assert!(undo(&dir).unwrap_err().to_string().starts_with("there is nothing to undo"));
        append(&dir, "a.mp4").unwrap();
        let (done, project) = append(&dir, "b.mp4").unwrap();
        assert_eq!(done, "Put b.mp4.");
        assert_eq!(laid_out(&project).len(), 2);
        assert_eq!(laid_out(&load(&dir).unwrap()).len(), 2, "saved");
        let (done, project) = undo(&dir).unwrap();
        assert_eq!(done, "Took back `append b.mp4`.");
        assert_eq!(laid_out(&project).len(), 1);
        assert!(undo(&dir).is_err(), "one step only");
        // A failed change writes nothing, and leaves nothing to undo.
        let failed = change(&dir, "append", |_| anyhow::bail!("no"));
        assert_eq!(failed.unwrap_err().to_string(), "no");
        assert_eq!(laid_out(&load(&dir).unwrap()).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_does_not_lose_what_changed_since() {
        let dir = temp_dir("since");
        create(&dir, &new_project("Lesson", PresetId::Landscape)).unwrap();
        append(&dir, "a.mp4").unwrap();
        // The editor saved the project in the meantime.
        let mut project = load(&dir).unwrap();
        project.view.playhead = 3.0;
        project.save(&dir).unwrap();
        assert!(undo(&dir).unwrap_err().to_string().starts_with("the project has changed since `append a.mp4`"));
        assert_eq!(laid_out(&load(&dir).unwrap()).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_project_open_in_the_editor_is_left_alone() {
        let dir = temp_dir("open");
        create(&dir, &new_project("Lesson", PresetId::Landscape)).unwrap();
        let lock = EditorLock::take(&dir).unwrap();
        assert!(append(&dir, "a.mp4").unwrap_err().to_string().starts_with("the project is open in the editor"));
        assert!(load(&dir).is_ok(), "reading is fine");
        drop(lock);
        append(&dir, "a.mp4").unwrap();
        assert_eq!(folder_of(&dir.join(PROJECT_FILE).to_string_lossy()), dir);
        let _ = fs::remove_dir_all(&dir);
    }
}
