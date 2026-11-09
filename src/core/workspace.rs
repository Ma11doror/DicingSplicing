//! The folder the user chose for everything the editor makes: projects, and a cache they share.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const MARKER_FILE: &str = "workspace.json";
const VERSION: u32 = 1;
const MAX_NAME_CHARS: usize = 60;

#[derive(Serialize, Deserialize)]
struct Marker {
    version: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workspace {
    pub root: PathBuf,
}

impl Workspace {
    /// Makes a folder a workspace, or opens it if it already is one.
    pub fn open_or_create(root: &Path) -> Result<Self> {
        let workspace = Self { root: root.to_owned() };
        fs::create_dir_all(workspace.projects_dir()).with_context(|| format!("can't create folders in {}", root.display()))?;
        fs::create_dir_all(workspace.cache_dir()).with_context(|| format!("can't create folders in {}", root.display()))?;
        let marker = root.join(MARKER_FILE);
        if !marker.exists() {
            fs::write(&marker, serde_json::to_vec_pretty(&Marker { version: VERSION })?).with_context(|| format!("can't write {}", marker.display()))?;
        }
        Ok(workspace)
    }

    /// The workspace a project is in, looking up from its folder.
    pub fn containing(dir: &Path) -> Option<Self> {
        dir.ancestors().find(|folder| folder.join(MARKER_FILE).is_file()).map(|root| Self { root: root.to_owned() })
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// Where a plugin keeps what it brings in. This is not cache: a project's video may be here.
    pub fn plugin_dir(&self, plugin: &str) -> PathBuf {
        self.root.join("downloads").join(folder_name(plugin))
    }

    /// Where a plugin keeps its own notes, apart from anything it brings in.
    pub fn plugin_data_dir(&self, plugin: &str) -> PathBuf {
        self.root.join("plugins").join(folder_name(plugin))
    }

    /// Creates a folder for a new project, named after it as far as that is free.
    pub fn new_project_dir(&self, name: &str) -> Result<PathBuf> {
        let base = folder_name(name);
        fs::create_dir_all(self.projects_dir())?;
        for n in 1..10_000 {
            let candidate = if n == 1 { base.clone() } else { format!("{base} {n}") };
            let dir = self.projects_dir().join(candidate);
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(dir),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("can't create {}", dir.display())),
            }
        }
        bail!("there are too many projects named {base}")
    }
}

/// A name that any file system takes. Titles of videos can contain just about anything.
pub fn folder_name(name: &str) -> String {
    let kept: String = name.chars().map(|c| if c.is_alphanumeric() || " -_.,()".contains(c) { c } else { ' ' }).collect();
    let words = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = words.chars().take(MAX_NAME_CHARS).collect();
    // Windows drops dots and spaces at the end, and a leading dot hides a folder elsewhere.
    let trimmed = short.trim_matches(|c: char| c == ' ' || c == '.');
    if trimmed.is_empty() {
        return "project".to_owned();
    }
    let stem = trimmed.split('.').next().unwrap_or_default().to_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.ends_with(|c: char| c.is_ascii_digit()));
    if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_workspace_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn creates_its_folders_and_can_be_found_from_inside() {
        let root = temp_dir("layout");
        let workspace = Workspace::open_or_create(&root).unwrap();
        assert!(root.join(MARKER_FILE).is_file());
        assert!(workspace.projects_dir().is_dir());
        assert!(workspace.cache_dir().is_dir());

        // Opening it again changes nothing.
        assert_eq!(Workspace::open_or_create(&root).unwrap(), workspace);

        let project = workspace.new_project_dir("clip").unwrap();
        assert_eq!(Workspace::containing(&project), Some(workspace));
        assert_eq!(Workspace::containing(&temp_dir("elsewhere").join("a")), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn projects_of_the_same_name_get_their_own_folders() {
        let root = temp_dir("names");
        let workspace = Workspace::open_or_create(&root).unwrap();
        let first = workspace.new_project_dir("clip").unwrap();
        let second = workspace.new_project_dir("clip").unwrap();
        assert_eq!(first.file_name().unwrap(), "clip");
        assert_eq!(second.file_name().unwrap(), "clip 2");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn folder_names_are_safe() {
        assert_eq!(folder_name("Песня - Исполнитель"), "Песня - Исполнитель");
        assert_eq!(folder_name("a/b\\c:d*e?f\"g<h>i|j"), "a b c d e f g h i j");
        assert_eq!(folder_name("..\\..\\evil"), "evil");
        assert_eq!(folder_name("  dots at the end... "), "dots at the end");
        assert_eq!(folder_name("???"), "project");
        assert_eq!(folder_name("CON"), "_CON");
        assert_eq!(folder_name("nul.mp4"), "_nul.mp4");
        assert_eq!(folder_name("com1"), "_com1");
        assert_eq!(folder_name("Console"), "Console");
        assert_eq!(folder_name(&"x".repeat(200)).chars().count(), MAX_NAME_CHARS);
    }
}
