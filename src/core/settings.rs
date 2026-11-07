//! The application's settings. They live outside the workspace, in the user's own folder: the
//! workspace may be shared.

use crate::core::files;
use crate::core::tools;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 20;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub workspace: Option<PathBuf>,
    /// Project folders, the last one opened first.
    pub recent: Vec<PathBuf>,
    /// Where the programs are, when they are not on the PATH. Empty means the PATH.
    pub ffmpeg: String,
    pub ffprobe: String,
    /// The options of each plugin, by its id. The editor keeps them and doesn't look into them.
    pub plugin_options: BTreeMap<String, BTreeMap<String, String>>,
    /// The name of the newest error report the user has been told about.
    pub crash_seen: Option<String>,
}

/// `%APPDATA%\simple_editor\settings.json`.
pub fn default_path() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("simple_editor").join("settings.json")
}

impl Settings {
    /// Reads the settings. A damaged file must not keep the editor from starting: it is put aside
    /// and the second value says so.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let Ok(bytes) = fs::read(path) else { return (Self::default(), None) };
        match serde_json::from_slice(&bytes) {
            Ok(settings) => (settings, None),
            Err(error) => {
                let aside = path.with_extension("json.bad");
                let _ = fs::rename(path, &aside);
                (Self::default(), Some(format!("The settings were damaged ({error}) and were put aside as {}.", aside.display())))
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        files::write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }

    /// Moves a project to the top of the recent ones.
    pub fn remember_project(&mut self, dir: &Path) {
        self.forget_project(dir);
        self.recent.insert(0, dir.to_owned());
        self.recent.truncate(MAX_RECENT);
    }

    pub fn forget_project(&mut self, dir: &Path) {
        self.recent.retain(|recent| recent != dir);
    }

    /// Makes the program paths take effect.
    pub fn apply_programs(&self) {
        tools::set_program(tools::FFMPEG, &self.ffmpeg);
        tools::set_program(tools::FFPROBE, &self.ffprobe);
    }

    /// What a plugin should be told: the options that are set.
    pub fn options_of(&self, plugin: &str) -> BTreeMap<String, String> {
        let options = self.plugin_options.get(plugin).into_iter().flatten();
        options.filter(|(_, value)| !value.trim().is_empty()).map(|(key, value)| (key.clone(), value.trim().to_owned())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("simple_editor_settings_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("settings.json")
    }

    #[test]
    fn settings_survive_a_restart() {
        let path = temp_file("restart");
        let settings = Settings { workspace: Some("C:/work".into()), recent: vec!["C:/work/projects/a".into()], ffmpeg: "C:/bin/ffmpeg.exe".into(), ..Settings::default() };
        settings.save(&path).unwrap();
        let (loaded, problem) = Settings::load(&path);
        assert_eq!(loaded, settings);
        assert_eq!(problem, None);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_file_is_a_first_start() {
        let (loaded, problem) = Settings::load(&temp_file("missing"));
        assert_eq!(loaded, Settings::default());
        assert_eq!(problem, None);
    }

    #[test]
    fn a_damaged_file_is_put_aside() {
        let path = temp_file("damaged");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();
        let (loaded, problem) = Settings::load(&path);
        assert_eq!(loaded, Settings::default());
        assert!(problem.is_some());
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(path.with_extension("json.bad")).unwrap(), "{ not json");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unknown_and_missing_fields_are_fine() {
        let settings: Settings = serde_json::from_str(r#"{"recent": ["a"], "from_a_newer_version": 1}"#).unwrap();
        assert_eq!(settings.recent, [PathBuf::from("a")]);
        assert_eq!(settings.workspace, None);
    }

    #[test]
    fn a_plugin_is_told_only_the_options_that_are_set() {
        let mut settings = Settings::default();
        let options = settings.plugin_options.entry("plugin".into()).or_default();
        options.insert("set".into(), " value ".into());
        options.insert("empty".into(), "  ".into());
        assert_eq!(settings.options_of("plugin"), BTreeMap::from([("set".to_owned(), "value".to_owned())]));
        assert!(settings.options_of("other").is_empty());
    }

    #[test]
    fn recent_projects_keep_their_order_and_limit() {
        let mut settings = Settings::default();
        for i in 0..MAX_RECENT + 5 {
            settings.remember_project(Path::new(&format!("p{i}")));
        }
        assert_eq!(settings.recent.len(), MAX_RECENT);
        assert_eq!(settings.recent[0], PathBuf::from(format!("p{}", MAX_RECENT + 4)));

        let old = settings.recent[5].clone();
        settings.remember_project(&old);
        assert_eq!(settings.recent[0], old);
        assert_eq!(settings.recent.len(), MAX_RECENT);

        settings.forget_project(&old);
        assert!(!settings.recent.contains(&old));
    }
}
