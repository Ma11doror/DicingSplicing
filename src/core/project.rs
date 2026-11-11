//! A project: what is edited, saved in a folder of its own.

use crate::core::edit::{Clip, Sequence, SourceId, TrackId};
use crate::core::files;
use crate::core::preset::{FrameOptions, PresetId};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const PROJECT_FILE: &str = "project.json";
pub const RENDERS_FOLDER: &str = "renders";
/// 2: several tracks, each with an id of its own. 3: effects on clips. 4: curves of fades. 5: colour.
/// 6: audio tracks, detached sound and chained clips. An editor that knows less can't open such a
/// file, and so can't lose what it does not know by saving it.
pub const VERSION: u32 = 6;

/// A video the project uses. It is not copied: the project remembers where it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub path: PathBuf,
    /// What it is called, when the file's name says little: a download is always `video.mp4`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What the plugin that brought the video in said about it, if one did.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
}

impl Source {
    /// What the user sees it as.
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| file_label(&self.path))
    }
}

/// A file's name, or the whole path if it has none.
pub fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// Adds a video to the sources, unless the same file is there already. Returns its id and whether
/// it is new.
pub fn add_source(sources: &mut Vec<Source>, path: PathBuf, name: Option<String>, meta: BTreeMap<String, String>) -> (SourceId, bool) {
    if let Some(existing) = sources.iter().find(|source| same_file(&source.path, &path)) {
        return (existing.id, false);
    }
    let id = sources.iter().map(|source| source.id + 1).max().unwrap_or(0);
    sources.push(Source { id, path, name, meta });
    (id, true)
}

/// Where the user left things. None of this changes the result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewState {
    pub playhead: f64,
    /// Pixels per second of the timeline, once it has been shown.
    pub timeline_scale: Option<f32>,
    pub frames: Vec<(PresetId, FrameOptions)>,
    pub ratio_locked: bool,
    /// The file to export to, when it is not the default one in the project's folder.
    pub export_path: Option<PathBuf>,
    pub export_shown_frames: bool,
    pub loop_export: bool,
    /// The tracks whose rows show their clips' effects and sound.
    pub expanded_tracks: Vec<TrackId>,
    /// How tall the clip's panel is, once its border has been dragged.
    pub clip_panel_height: Option<f32>,
    /// How tall the timeline is, once its border has been dragged.
    pub timeline_height: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub name: String,
    pub sources: Vec<Source>,
    pub sequence: Sequence,
    #[serde(default)]
    pub view: ViewState,
}

impl Project {
    /// A new project of one video, all of it on the timeline.
    pub fn new(name: String, video: PathBuf, duration: f64) -> Self {
        Self {
            version: VERSION,
            sources: vec![Source { id: 0, path: video, name: Some(name.clone()), meta: BTreeMap::new() }],
            sequence: Sequence::new(Clip::new(name.clone(), duration)),
            name,
            view: ViewState::default(),
        }
    }

    /// Reads the project in `dir`. A file that can't be read stays exactly as it is.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(PROJECT_FILE);
        let bytes = fs::read(&path).with_context(|| format!("can't read {}", path.display()))?;
        Self::parse(&bytes).with_context(|| format!("can't open {}", path.display()))
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct Header {
            version: u32,
        }
        let header: Header = serde_json::from_slice(bytes).context("the file is damaged")?;
        if header.version > VERSION {
            bail!("it was saved by a newer version of the editor (format {}, this one reads up to {VERSION})", header.version);
        }
        let mut project: Self = serde_json::from_slice(bytes).context("the file is damaged")?;
        // What came from the first version: one track with no id, a source without a name. The
        // project and its clip were named after the video then.
        project.version = VERSION;
        project.sequence.number_tracks();
        for source in project.sources.iter_mut().filter(|source| source.name.is_none()) {
            source.name = project.sequence.clips().find(|(_, clip)| clip.source == source.id).map(|(_, clip)| clip.name.clone());
        }
        Ok(project)
    }

    /// Writes the project into `dir`. What is written is read back first, so that a value JSON
    /// can't hold never replaces a good file with one that won't open.
    pub fn save(&self, dir: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        Self::parse(&bytes).context("the project can't be saved")?;
        files::write_atomic(&dir.join(PROJECT_FILE), &bytes)
    }

    /// The name of the project in `dir`, without reading the rest.
    pub fn name_in(dir: &Path) -> Option<String> {
        #[derive(Deserialize)]
        struct Named {
            name: String,
        }
        let bytes = fs::read(dir.join(PROJECT_FILE)).ok()?;
        serde_json::from_slice::<Named>(&bytes).ok().map(|named| named.name)
    }
}

/// Whether two paths lead to the same file, however they are written.
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The folder of a project in `projects_dir` that was made from this video.
pub fn find_by_video(projects_dir: &Path, video: &Path) -> Option<PathBuf> {
    let folders = fs::read_dir(projects_dir).ok()?;
    folders
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|dir| Project::load(dir).is_ok_and(|project| project.sources.iter().any(|source| same_file(&source.path, video))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::{ClipId, Span};
    use crate::core::export::Rate;
    use crate::core::preset::Ratio;
    use crate::media::geometry::Transform;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_project_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A project with something non-default in every part of it.
    fn edited() -> Project {
        let mut project = Project::new("clip".into(), "C:/videos/clip.mp4".into(), 30.0);
        let id = ClipId { track: 0, index: 0 };
        let sequence = &mut project.sequence;
        sequence.trim_start(id, 2.0, 0.1);
        sequence.trim_end(id, 20.0, 0.1);
        sequence.export = Span { start: 3.0, end: 15.5 };
        sequence.output.preset = PresetId::Custom;
        sequence.output.custom = Ratio::new(5.0, 4.0);
        sequence.settings.rate = Rate::Bitrate(4000);
        sequence.settings.fades = (0.5, 2.0);
        let clip = sequence.clip_mut(id);
        clip.transform = Transform { cx: 0.25, cy: 0.75, zoom: 1.5, angle_deg: -33.5, mirror: true, bars: true };
        clip.volume = 0.5;
        clip.muted = true;
        clip.add_fade(crate::core::edit::Edge::Out);
        project.view = ViewState {
            playhead: 7.25,
            timeline_scale: Some(44.0),
            frames: vec![(PresetId::Square, FrameOptions { shown: true, thirds: false, center: true, safe: false })],
            ratio_locked: true,
            export_path: Some("D:/out/final.mp4".into()),
            export_shown_frames: true,
            loop_export: true,
            expanded_tracks: vec![0],
            clip_panel_height: Some(280.0),
            timeline_height: Some(360.0),
        };
        project
    }

    #[test]
    fn everything_comes_back() {
        let dir = temp_dir("roundtrip");
        let project = edited();
        project.save(&dir).unwrap();
        assert_eq!(Project::load(&dir).unwrap(), project);
        assert_eq!(Project::name_in(&dir).as_deref(), Some("clip"));
        // Only the project file: the temporary one is gone.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_is_reported_and_not_touched() {
        let dir = temp_dir("damaged");
        let path = dir.join(PROJECT_FILE);
        fs::write(&path, "{ \"version\": 1, \"name\": ").unwrap();
        let error = format!("{:#}", Project::load(&dir).unwrap_err());
        assert!(error.contains("damaged"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ \"version\": 1, \"name\": ");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_of_the_first_version_opens() {
        let mut value = serde_json::to_value(edited()).unwrap();
        value["version"] = 1.into();
        value["sources"][0].as_object_mut().unwrap().remove("name");
        value["sequence"]["tracks"][0].as_object_mut().unwrap().remove("id");
        let mut second = value["sequence"]["tracks"][0].clone();
        second["clips"] = serde_json::json!([]);
        value["sequence"]["tracks"].as_array_mut().unwrap().push(second);

        let project = Project::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(project.version, VERSION);
        assert_eq!(project.sources[0].label(), "clip");
        assert_eq!(project.sequence.tracks.iter().map(|track| track.id).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(project.sequence.clips().count(), 1);
    }

    #[test]
    fn a_file_is_added_once() {
        let dir = temp_dir("pool");
        let (a, b) = (dir.join("a.mp4"), dir.join("b.mp4"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        let mut sources = Project::new("first".into(), a.clone(), 1.0).sources;
        assert_eq!(add_source(&mut sources, b.clone(), None, BTreeMap::new()), (1, true));
        // The same file, written another way.
        let again = dir.join(".").join("a.mp4");
        assert_eq!(add_source(&mut sources, again, Some("x".into()), BTreeMap::new()), (0, false));
        assert_eq!(sources.len(), 2);
        assert_eq!((sources[0].label(), sources[1].label()), ("first".to_owned(), "b.mp4".to_owned()));
        // Ids are not given twice, even after a source is gone.
        sources.remove(0);
        assert_eq!(add_source(&mut sources, a, None, BTreeMap::new()), (2, true));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_format_is_refused_with_a_reason() {
        let dir = temp_dir("newer");
        fs::write(dir.join(PROJECT_FILE), format!("{{\"version\": {}, \"something_new\": true}}", VERSION + 1)).unwrap();
        let error = format!("{:#}", Project::load(&dir).unwrap_err());
        assert!(error.contains("newer version"), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_value_json_cant_hold_does_not_replace_a_good_file() {
        let dir = temp_dir("nan");
        let good = edited();
        good.save(&dir).unwrap();

        let mut broken = good.clone();
        broken.sequence.export.end = f64::NAN;
        assert!(broken.save(&dir).is_err());
        assert_eq!(Project::load(&dir).unwrap(), good);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fields_added_later_have_defaults() {
        // A file without the view, and a clip without the fields that came with sound and sources.
        let mut value = serde_json::to_value(edited()).unwrap();
        value.as_object_mut().unwrap().remove("view");
        let clip = value["sequence"]["tracks"][0]["clips"][0].as_object_mut().unwrap();
        clip.remove("volume");
        clip.remove("muted");
        clip.remove("source");
        let project = Project::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(project.view, ViewState::default());
        let clip = project.sequence.clip(ClipId { track: 0, index: 0 });
        assert_eq!((clip.volume, clip.muted, clip.source), (1.0, false, 0));
    }

    #[test]
    fn finds_the_project_of_a_video() {
        let dir = temp_dir("find");
        let video = dir.join("video.mp4");
        fs::write(&video, "video").unwrap();
        let other = dir.join("other.mp4");
        fs::write(&other, "other").unwrap();

        let projects = dir.join("projects");
        let a = projects.join("a");
        fs::create_dir_all(&a).unwrap();
        Project::new("a".into(), other.clone(), 1.0).save(&a).unwrap();
        let b = projects.join("b");
        fs::create_dir_all(&b).unwrap();
        Project::new("b".into(), video.clone(), 1.0).save(&b).unwrap();
        // Something that is not a project doesn't get in the way.
        fs::create_dir_all(projects.join("c")).unwrap();

        assert_eq!(find_by_video(&projects, &video), Some(b));
        assert_eq!(find_by_video(&projects, &other), Some(a));
        assert_eq!(find_by_video(&projects, &dir.join("new.mp4")), None);
        assert_eq!(find_by_video(&dir.join("no_such_folder"), &video), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
