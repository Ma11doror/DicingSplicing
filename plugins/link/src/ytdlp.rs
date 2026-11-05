//! Running yt-dlp.

use crate::cancel::{hide_window, Cancel};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

/// What yt-dlp takes as "the first result of a YouTube search for this".
pub const SEARCH_PREFIX: &str = "ytsearch1:";
const FILE_STEM: &str = "video";
const ERROR_LINES: usize = 8;
const PROGRESS_PREFIX: &str = "SE|";

/// The first file usually is the picture and the second the sound, the larger part of the work.
const PICTURE_SHARE: f32 = 0.75;
const SOUND_SHARE: f32 = 0.2;

const PROGRAM_NAME: &str = "yt-dlp";
const PROGRAM_FILE: &str = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };

pub struct YtDlp {
    pub program: PathBuf,
    pub cookies: Option<PathBuf>,
    pub max_height: u32,
    pub ffmpeg: Option<String>,
}

/// The program to run when the settings don't say which.
pub fn find_program() -> PathBuf {
    locate(&|name| std::env::var_os(name))
}

/// On the PATH it goes by its name, as that is the user's own choice. Otherwise it is looked for
/// where it usually gets installed: the editor may have been started before the install changed
/// the PATH, or from somewhere that never saw it. Found nowhere, the name stays, and running it
/// says that it is missing.
fn locate(env: &dyn Fn(&str) -> Option<OsString>) -> PathBuf {
    let on_path = env("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(PROGRAM_FILE).is_file()));
    if on_path {
        return PathBuf::from(PROGRAM_NAME);
    }
    usual_places(env).into_iter().find(|place| place.is_file()).unwrap_or_else(|| PathBuf::from(PROGRAM_NAME))
}

/// Where the ways of installing it leave the program.
#[cfg(windows)]
fn usual_places(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let dir = |name: &str| env(name).map(PathBuf::from);
    let local = dir("LOCALAPPDATA");
    let program_data = dir("ProgramData");
    let home = dir("USERPROFILE");

    let mut folders: Vec<PathBuf> = Vec::new();
    // winget: a link if it could make one, and the package's own folder in any case.
    let winget = [local.as_ref().map(|d| d.join("Microsoft").join("WinGet")), dir("ProgramFiles").map(|d| d.join("WinGet"))];
    for root in winget.into_iter().flatten() {
        folders.push(root.join("Links"));
        folders.extend(subfolders(&root.join("Packages"), "yt-dlp.yt-dlp"));
    }
    // Chocolatey and Scoop, which can be told to live elsewhere.
    let chocolatey = dir("ChocolateyInstall").or_else(|| program_data.as_ref().map(|d| d.join("chocolatey")));
    folders.extend(chocolatey.map(|d| d.join("bin")));
    let scoop = dir("SCOOP").or_else(|| home.as_ref().map(|d| d.join("scoop")));
    folders.extend(scoop.map(|d| d.join("shims")));
    folders.extend(program_data.as_ref().map(|d| d.join("scoop").join("shims")));
    // pip, for all users of a Python and with --user, one folder per version. pipx.
    if let Some(local) = &local {
        folders.extend(subfolders(&local.join("Programs").join("Python"), "Python").into_iter().map(|d| d.join("Scripts")));
    }
    if let Some(roaming) = dir("APPDATA") {
        folders.extend(subfolders(&roaming.join("Python"), "Python").into_iter().map(|d| d.join("Scripts")));
    }
    folders.extend(home.as_ref().map(|d| d.join(".local").join("bin")));

    folders.into_iter().map(|folder| folder.join(PROGRAM_FILE)).collect()
}

/// Started from a desktop, a program may not get the PATH of the terminal.
#[cfg(not(windows))]
fn usual_places(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let home = env("HOME").map(|d| PathBuf::from(d).join(".local").join("bin"));
    let system = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from);
    home.into_iter().chain(system).map(|folder| folder.join(PROGRAM_FILE)).collect()
}

/// The folders in `dir` whose names start with `prefix`, sorted so that the same one is always found.
#[cfg(windows)]
fn subfolders(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<PathBuf> =
        entries.filter_map(Result::ok).filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix)).map(|entry| entry.path()).collect();
    found.sort();
    found
}

/// What a video is, as yt-dlp sees it.
#[derive(Debug, PartialEq)]
pub struct VideoInfo {
    pub id: String,
    pub extractor: String,
    pub title: String,
    pub url: Option<String>,
}

impl VideoInfo {
    /// The name of the video's folder: the same video always gets the same one, and nothing in a
    /// video's id can lead outside of it.
    pub fn key(&self) -> String {
        let safe = |text: &str| -> String {
            text.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
        };
        format!("{}-{}", safe(&self.extractor).to_lowercase(), safe(&self.id))
    }

    pub fn url_or(&self, target: &str) -> String {
        self.url.clone().unwrap_or_else(|| target.to_owned())
    }
}

/// Reads what `yt-dlp -J` says. A search or a playlist comes as a list: its first video counts.
fn parse_info(json: &str) -> Result<VideoInfo> {
    let value: Value = serde_json::from_str(json).context("yt-dlp said something unexpected")?;
    let video = match value.get("entries") {
        Some(entries) => entries.as_array().and_then(|entries| entries.iter().find(|entry| !entry.is_null())).ok_or_else(|| anyhow!("Nothing was found for this link"))?,
        None => &value,
    };
    let text = |key: &str| video.get(key).and_then(Value::as_str).map(str::to_owned);
    Ok(VideoInfo {
        id: text("id").context("yt-dlp gave no id for the video")?,
        extractor: text("extractor_key").or_else(|| text("extractor")).unwrap_or_else(|| "video".to_owned()),
        title: text("title").unwrap_or_else(|| "video".to_owned()),
        url: text("webpage_url"),
    })
}

/// The picture no higher than the limit, with the best sound; whatever there is, if that is not possible.
fn format_selection(max_height: u32) -> String {
    format!("bv*[height<={max_height}]+ba/b[height<={max_height}]/bv*+ba/b")
}

/// How far along a progress line says the current file is.
fn parse_progress(line: &str) -> Option<f32> {
    let mut parts = line.strip_prefix(PROGRESS_PREFIX)?.split('|');
    let (done, total, estimate) = (parts.next()?, parts.next()?, parts.next()?);
    let done: f64 = done.parse().ok()?;
    let total: f64 = total.parse().or_else(|_| estimate.parse()).ok()?;
    (total > 0.0).then(|| ((done / total) as f32).clamp(0.0, 1.0))
}

/// Turns yt-dlp's lines into progress over the whole download.
#[derive(Default)]
pub struct ProgressTracker {
    file: u32,
    in_file: f32,
    last_percent: Option<i32>,
    merging: bool,
}

impl ProgressTracker {
    /// The whole download's fraction and what is going on, when there is something new to say.
    pub fn update(&mut self, line: &str) -> Option<(f32, &'static str)> {
        if line.starts_with("[Merger]") || line.starts_with("[VideoConvertor]") || line.starts_with("[VideoRemuxer]") {
            self.merging = true;
            return Some((PICTURE_SHARE + SOUND_SHARE, "Putting the picture and the sound together"));
        }
        let in_file = parse_progress(line)?;
        // A file starting over from the beginning is the next one.
        if in_file + 0.5 < self.in_file {
            self.file += 1;
        }
        self.in_file = in_file;
        let (fraction, text) = if self.file == 0 {
            (in_file * PICTURE_SHARE, "Downloading")
        } else {
            (PICTURE_SHARE + in_file * SOUND_SHARE, "Downloading the sound")
        };
        let percent = (fraction * 100.0) as i32;
        if self.last_percent == Some(percent) {
            return None;
        }
        self.last_percent = Some(percent);
        Some((fraction, text))
    }
}

/// The finished video in a download's folder, not one of the pieces it was made from.
pub fn downloaded_file(dir: &Path) -> Option<PathBuf> {
    let is_final = |path: &Path| {
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
        path.file_stem().is_some_and(|stem| stem == FILE_STEM) && !matches!(extension, "part" | "ytdl" | "temp" | "tmp" | "")
    };
    fs::read_dir(dir).ok()?.filter_map(Result::ok).map(|entry| entry.path()).find(|path| path.is_file() && is_final(path))
}

/// The most telling lines of what yt-dlp wrote to its error stream.
fn error_message(errors: &[String]) -> String {
    let marked: Vec<&str> = errors.iter().filter(|line| line.starts_with("ERROR")).map(String::as_str).collect();
    if marked.is_empty() {
        errors.join("\n")
    } else {
        marked.join("\n")
    }
}

impl YtDlp {
    fn common_args(&self, with_cookies: bool) -> Vec<String> {
        let mut args: Vec<String> = ["--no-playlist", "--no-warnings"].map(str::to_owned).to_vec();
        if let (true, Some(cookies)) = (with_cookies, &self.cookies) {
            args.push("--cookies".to_owned());
            args.push(cookies.display().to_string());
        }
        if let Some(ffmpeg) = &self.ffmpeg {
            args.push("--ffmpeg-location".to_owned());
            args.push(ffmpeg.clone());
        }
        args
    }

    /// Runs `yt-dlp`, handing each line it prints to `on_line`.
    fn run(&self, args: &[String], cancel: &Cancel, on_line: &mut dyn FnMut(&str)) -> Result<()> {
        if cancel.is_cancelled() {
            bail!("Cancelled");
        }
        let mut command = Command::new(&self.program);
        command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide_window(&mut command);
        let mut child =
            command.spawn().with_context(|| format!("{} was not found: install it, or say where it is in the settings of this plugin", self.program.display()))?;
        cancel.started(child.id());

        let stderr = child.stderr.take().context("yt-dlp has no error stream")?;
        let errors = thread::spawn(move || {
            let mut last: Vec<String> = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if last.len() == ERROR_LINES {
                    last.remove(0);
                }
                last.push(line);
            }
            last
        });
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                on_line(&line);
            }
        }
        let status = child.wait();
        cancel.finished();
        let errors = errors.join().unwrap_or_default();
        if cancel.is_cancelled() {
            bail!("Cancelled");
        }
        match status {
            Ok(status) if status.success() => Ok(()),
            _ if errors.is_empty() => bail!("yt-dlp stopped with an error"),
            _ => bail!("{}", error_message(&errors)),
        }
    }

    /// Tries without the cookies first: stale ones make YouTube refuse even public videos.
    fn with_cookies_as_fallback<T>(&self, cancel: &Cancel, mut attempt: impl FnMut(bool) -> Result<T>) -> Result<T> {
        match attempt(false) {
            Ok(value) => Ok(value),
            Err(_) if !cancel.is_cancelled() && self.cookies.as_deref().is_some_and(Path::is_file) => attempt(true),
            Err(error) => Err(error),
        }
    }

    pub fn inspect(&self, target: &str, cancel: &Cancel) -> Result<VideoInfo> {
        self.with_cookies_as_fallback(cancel, |with_cookies| {
            let mut args = self.common_args(with_cookies);
            args.push("-J".to_owned());
            args.push("--".to_owned());
            args.push(target.to_owned());
            let mut json = String::new();
            self.run(&args, cancel, &mut |line| json.push_str(line))?;
            parse_info(&json)
        })
    }

    fn download_args(&self, url: &str, dir: &Path, with_cookies: bool) -> Vec<String> {
        let mut args = self.common_args(with_cookies);
        let template = format!("download:{PROGRESS_PREFIX}%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s");
        // The folder goes separately: a "%" in its name would mean something in the file name template.
        args.extend(
            ["-f", &format_selection(self.max_height), "--merge-output-format", "mp4", "--newline", "--progress-template", &template, "-P"]
                .map(str::to_owned),
        );
        args.push(dir.display().to_string());
        args.push("-o".to_owned());
        args.push(format!("{FILE_STEM}.%(ext)s"));
        // Whatever the link is, it is not an option.
        args.push("--".to_owned());
        args.push(url.to_owned());
        args
    }

    pub fn download(&self, url: &str, dir: &Path, cancel: &Cancel, on_line: &mut dyn FnMut(&str)) -> Result<()> {
        self.with_cookies_as_fallback(cancel, |with_cookies| self.run(&self.download_args(url, dir, with_cookies), cancel, on_line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> YtDlp {
        YtDlp { program: "yt-dlp".into(), cookies: None, max_height: 720, ffmpeg: None }
    }

    /// An empty folder of its own for one test.
    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("simple_editor_link_find_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Puts a program at `relative` inside `root`, and says where that is.
    fn install(root: &Path, relative: &str) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "x").unwrap();
        path
    }

    /// `locate` in a world that has only these variables.
    fn locate_with(vars: &[(&str, &Path)]) -> PathBuf {
        locate(&|name| vars.iter().find(|(var, _)| *var == name).map(|(_, value)| value.as_os_str().to_owned()))
    }

    #[cfg(windows)]
    #[test]
    fn finds_what_each_way_of_installing_leaves() {
        let cases = [
            ("LOCALAPPDATA", "Microsoft/WinGet/Links/yt-dlp.exe"),
            ("LOCALAPPDATA", "Microsoft/WinGet/Packages/yt-dlp.yt-dlp_Microsoft.Winget.Source_8wekyb3d8bbwe/yt-dlp.exe"),
            ("ProgramFiles", "WinGet/Links/yt-dlp.exe"),
            ("ProgramFiles", "WinGet/Packages/yt-dlp.yt-dlp_Microsoft.Winget.Source_8wekyb3d8bbwe/yt-dlp.exe"),
            ("ProgramData", "chocolatey/bin/yt-dlp.exe"),
            ("ChocolateyInstall", "bin/yt-dlp.exe"),
            ("USERPROFILE", "scoop/shims/yt-dlp.exe"),
            ("SCOOP", "shims/yt-dlp.exe"),
            ("ProgramData", "scoop/shims/yt-dlp.exe"),
            ("LOCALAPPDATA", "Programs/Python/Python312/Scripts/yt-dlp.exe"),
            ("APPDATA", "Python/Python313/Scripts/yt-dlp.exe"),
            ("USERPROFILE", ".local/bin/yt-dlp.exe"),
        ];
        for (n, (var, relative)) in cases.into_iter().enumerate() {
            let root = temp_root(&format!("way{n}"));
            let path = install(&root, relative);
            assert_eq!(locate_with(&[(var, &root)]), path, "{var} {relative}");
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_path_comes_before_the_usual_places() {
        let root = temp_root("path");
        let on_path = root.join("on path");
        install(&on_path, PROGRAM_FILE);
        install(&root, "Microsoft/WinGet/Links/yt-dlp.exe");
        install(&root, ".local/bin/yt-dlp");
        let found = locate_with(&[("PATH", &on_path), ("LOCALAPPDATA", &root), ("HOME", &root)]);
        assert_eq!(found, Path::new("yt-dlp"));
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn a_path_without_it_does_not_stop_the_search() {
        let root = temp_root("other_path");
        let path = install(&root, "Microsoft/WinGet/Links/yt-dlp.exe");
        let empty = root.join("nothing here");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(locate_with(&[("PATH", &empty), ("LOCALAPPDATA", &root)]), path);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn found_nowhere_it_stays_a_name() {
        let root = temp_root("nowhere");
        assert_eq!(locate_with(&[]), Path::new("yt-dlp"));
        assert_eq!(locate_with(&[("PATH", &root), ("LOCALAPPDATA", &root), ("USERPROFILE", &root)]), Path::new("yt-dlp"));

        // winget's other package, a folder with the program's name: neither of them is the program.
        install(&root, "Microsoft/WinGet/Packages/yt-dlp.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe/yt-dlp.exe");
        fs::create_dir_all(root.join("Microsoft/WinGet/Links/yt-dlp.exe")).unwrap();
        assert_eq!(locate_with(&[("LOCALAPPDATA", &root)]), Path::new("yt-dlp"));
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn the_order_of_the_places_is_always_the_same() {
        let root = temp_root("order");
        // Of two Pythons, the same one each time.
        let python = install(&root, "Programs/Python/Python312/Scripts/yt-dlp.exe");
        install(&root, "Programs/Python/Python313/Scripts/yt-dlp.exe");
        assert_eq!(locate_with(&[("LOCALAPPDATA", &root)]), python);

        // winget before pip, whichever came first.
        let winget = install(&root, "Microsoft/WinGet/Packages/yt-dlp.yt-dlp_x/yt-dlp.exe");
        assert_eq!(locate_with(&[("LOCALAPPDATA", &root)]), winget);
        let links = install(&root, "Microsoft/WinGet/Links/yt-dlp.exe");
        assert_eq!(locate_with(&[("LOCALAPPDATA", &root)]), links);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_a_single_video() {
        let info = parse_info(r#"{"id":"jNQXAC9IVRw","extractor_key":"Youtube","title":"Me at the zoo","webpage_url":"https://www.youtube.com/watch?v=jNQXAC9IVRw"}"#).unwrap();
        assert_eq!(info.title, "Me at the zoo");
        assert_eq!(info.key(), "youtube-jNQXAC9IVRw");
        assert_eq!(info.url_or("x"), "https://www.youtube.com/watch?v=jNQXAC9IVRw");
    }

    #[test]
    fn a_search_gives_its_first_video() {
        let info = parse_info(r#"{"_type":"playlist","id":"query","entries":[null,{"id":"abc","extractor":"youtube","title":"T"}]}"#).unwrap();
        assert_eq!(info.id, "abc");
        assert_eq!(info.key(), "youtube-abc");
        assert_eq!(info.url_or("target"), "target");

        let error = parse_info(r#"{"_type":"playlist","entries":[]}"#).unwrap_err();
        assert!(error.to_string().contains("Nothing was found"));
        assert!(parse_info("not json").is_err());
    }

    #[test]
    fn a_video_id_cant_lead_outside_its_folder() {
        let info = VideoInfo { id: "../../evil:1".into(), extractor: "Generic/x".into(), title: String::new(), url: None };
        assert_eq!(info.key(), "generic_x-______evil_1");
    }

    #[test]
    fn the_height_limit_goes_into_the_format() {
        assert_eq!(format_selection(1080), "bv*[height<=1080]+ba/b[height<=1080]/bv*+ba/b");
    }

    #[test]
    fn a_link_can_never_be_taken_for_an_option() {
        let args = tool().download_args("--exec=calc", Path::new("d"), false);
        let separator = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[separator + 1], "--exec=calc");
        assert_eq!(separator + 2, args.len());
    }

    #[test]
    fn cookies_go_in_only_when_asked_for() {
        let tool = YtDlp { cookies: Some("c.txt".into()), ..tool() };
        assert!(!tool.download_args("u", Path::new("d"), false).contains(&"--cookies".to_owned()));
        assert!(tool.download_args("u", Path::new("d"), true).contains(&"--cookies".to_owned()));
    }

    #[test]
    fn the_second_try_uses_the_cookies_if_they_exist() {
        let dir = std::env::temp_dir().join(format!("simple_editor_link_cookies_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cookies = dir.join("cookies.txt");
        fs::write(&cookies, "x").unwrap();
        let cancel = Cancel::default();

        // Without the file there is only the one try.
        let no_file = YtDlp { cookies: Some(dir.join("none.txt")), ..tool() };
        let mut tries = Vec::new();
        let result: Result<()> = no_file.with_cookies_as_fallback(&cancel, |with| {
            tries.push(with);
            bail!("no")
        });
        assert!(result.is_err());
        assert_eq!(tries, [false]);

        let with_file = YtDlp { cookies: Some(cookies), ..tool() };
        let mut tries = Vec::new();
        let result = with_file.with_cookies_as_fallback(&cancel, |with| {
            tries.push(with);
            if with {
                Ok(1)
            } else {
                bail!("no")
            }
        });
        assert_eq!(result.unwrap(), 1);
        assert_eq!(tries, [false, true]);

        // Working without them, they are left alone; cancelled, nothing is tried again.
        let mut tries = Vec::new();
        with_file.with_cookies_as_fallback(&cancel, |with| {
            tries.push(with);
            Ok(())
        })
        .unwrap();
        assert_eq!(tries, [false]);
        cancel.trigger();
        let mut tries = Vec::new();
        let result: Result<()> = with_file.with_cookies_as_fallback(&cancel, |with| {
            tries.push(with);
            bail!("no")
        });
        assert!(result.is_err());
        assert_eq!(tries, [false]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_progress_lines() {
        assert_eq!(parse_progress("SE|50|200|NA"), Some(0.25));
        assert_eq!(parse_progress("SE|50|NA|100.0"), Some(0.5));
        assert_eq!(parse_progress("SE|NA|NA|NA"), None);
        assert_eq!(parse_progress("[download] something"), None);
    }

    #[test]
    fn progress_covers_both_files_and_the_merge() {
        let mut tracker = ProgressTracker::default();
        assert_eq!(tracker.update("[youtube] Extracting URL"), None);
        assert_eq!(tracker.update("SE|50|100|NA"), Some((PICTURE_SHARE / 2.0, "Downloading")));
        // Nothing new to say until the percent changes.
        assert_eq!(tracker.update("SE|50|100|NA"), None);
        assert_eq!(tracker.update("SE|100|100|NA"), Some((PICTURE_SHARE, "Downloading")));

        let (fraction, text) = tracker.update("SE|10|100|NA").unwrap();
        assert!((fraction - (PICTURE_SHARE + 0.1 * SOUND_SHARE)).abs() < 1e-6);
        assert_eq!(text, "Downloading the sound");

        let (fraction, _) = tracker.update("[Merger] Merging formats into \"video.mp4\"").unwrap();
        assert!((fraction - 0.95).abs() < 1e-6);
    }

    #[test]
    fn only_the_finished_video_counts() {
        let dir = std::env::temp_dir().join(format!("simple_editor_link_files_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for piece in ["video.f137.mp4", "video.f137.mp4.part", "video.f140.m4a", "video.temp", "info.json"] {
            fs::write(dir.join(piece), "x").unwrap();
        }
        assert_eq!(downloaded_file(&dir), None);
        fs::write(dir.join("video.mp4"), "x").unwrap();
        assert_eq!(downloaded_file(&dir), Some(dir.join("video.mp4")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_say_what_matters() {
        let lines = ["WARNING: something".to_owned(), "ERROR: [youtube] x: Video unavailable".to_owned()];
        assert_eq!(error_message(&lines), "ERROR: [youtube] x: Video unavailable");
        assert_eq!(error_message(&lines[..1]), "WARNING: something");
    }
}
