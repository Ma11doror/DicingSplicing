//! A plugin for Simple Editor: downloads the video behind a link. song.link links are first resolved
//! to a video, anything else goes straight to yt-dlp.
//!
//! It is a program on its own and knows nothing of the editor except the plugin protocol, described
//! in the editor's `src/core/plugins.rs`. To try it by hand:
//!
//!     simple_editor-link fetch --input <link> --out <folder> [--option key=value]...
//!
//! It answers with one JSON object per line. Closing its standard input cancels the download.

mod cancel;
mod songlink;
mod ytdlp;

use anyhow::{bail, Context, Result};
use cancel::Cancel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use ytdlp::{ProgressTracker, YtDlp};

const DEFAULT_MAX_HEIGHT: u32 = 1080;
const MARKER_FILE: &str = "info.json";

const DESCRIPTION: &str = r#"{
  "protocol": 1,
  "kind": "source",
  "name": "Download from a link",
  "prompt": "A link to a song or a video",
  "hint": "song.link, YouTube and whatever else yt-dlp understands. A video is downloaded only once.",
  "options": [
    { "key": "yt_dlp", "label": "yt-dlp", "kind": "file", "hint": "From the PATH" },
    { "key": "cookies", "label": "Cookies file", "kind": "file", "hint": "Only if it fails without them" },
    { "key": "max_height", "label": "Highest quality", "kind": "text", "hint": "1080" }
  ]
}"#;

/// What this program says while it works.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
enum Event<'a> {
    Progress { fraction: Option<f32>, text: &'a str },
    Done { path: &'a Path, name: &'a str, meta: &'a BTreeMap<String, String> },
    Error { message: String },
}

fn emit(event: &Event) {
    if let Ok(line) = serde_json::to_string(event) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

fn say(text: &str) {
    emit(&Event::Progress { fraction: None, text });
}

#[derive(Debug, PartialEq)]
struct FetchArgs {
    input: String,
    out: PathBuf,
    options: BTreeMap<String, String>,
}

fn parse_fetch_args(args: &[String]) -> Result<FetchArgs> {
    let (mut input, mut out, mut options) = (None, None, BTreeMap::new());
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--input" => input = Some(value()?.clone()),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--option" => {
                let option = value()?;
                let (key, value) = option.split_once('=').with_context(|| format!("--option {option}: it should be key=value"))?;
                options.insert(key.to_owned(), value.to_owned());
            }
            other => bail!("I don't know {other}"),
        }
    }
    Ok(FetchArgs { input: input.context("--input is missing")?, out: out.context("--out is missing")?, options })
}

/// What is kept next to a finished download: it says the download is complete, and what it was.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Marker {
    file: PathBuf,
    name: String,
    meta: BTreeMap<String, String>,
}

/// The finished download in `dir`, if there is one whose file is still there.
fn finished(dir: &Path) -> Option<Marker> {
    let mut marker: Marker = serde_json::from_slice(&fs::read(dir.join(MARKER_FILE)).ok()?).ok()?;
    marker.file = dir.join(&marker.file);
    marker.file.is_file().then_some(marker)
}

fn write_marker(dir: &Path, marker: &Marker) -> Result<()> {
    // Only the file's name: the whole folder may move along with its workspace.
    let file = PathBuf::from(marker.file.file_name().context("the video has no file name")?);
    let stored = Marker { file, name: marker.name.clone(), meta: marker.meta.clone() };
    // In one step, as this is what makes the download count as complete.
    let temp = dir.join(format!("{MARKER_FILE}.tmp"));
    fs::write(&temp, serde_json::to_vec_pretty(&stored)?)?;
    fs::rename(&temp, dir.join(MARKER_FILE))?;
    Ok(())
}

/// A title for the project, the way tgCircleGen named its files.
fn display_name(song: &songlink::Song, video_title: &str) -> String {
    match (&song.title, &song.artist) {
        (Some(title), Some(artist)) => format!("{title} by {artist}"),
        (Some(title), None) => title.clone(),
        _ => video_title.to_owned(),
    }
}

fn check_link(input: &str) -> Result<&str> {
    let link = input.trim();
    if !(link.starts_with("http://") || link.starts_with("https://")) {
        bail!("This doesn't look like a link: it should start with http:// or https://");
    }
    Ok(link)
}

fn fetch(args: &FetchArgs, cancel: &Cancel) -> Result<Marker> {
    let link = check_link(&args.input)?;
    let option = |key: &str| args.options.get(key).map(|value| value.trim()).filter(|value| !value.is_empty());
    let max_height = match option("max_height") {
        Some(value) => value.parse().with_context(|| format!("The highest quality should be a number, like 1080, not {value}"))?,
        None => DEFAULT_MAX_HEIGHT,
    };
    let tool = YtDlp {
        program: option("yt_dlp").unwrap_or("yt-dlp").to_owned(),
        cookies: option("cookies").map(PathBuf::from),
        max_height,
        ffmpeg: std::env::var("SIMPLE_EDITOR_FFMPEG").ok().filter(|path| !path.is_empty() && path != "ffmpeg"),
    };

    let song = if songlink::is_song_link(link) {
        say("Asking song.link");
        songlink::resolve(link)
    } else {
        songlink::Song::default()
    };
    let target = song.download_target(link);
    if target.starts_with(ytdlp::SEARCH_PREFIX) {
        say("song.link gives no video: searching YouTube by the name");
    } else {
        say("Looking at the video");
    }
    let info = tool.inspect(&target, cancel)?;

    let dir = args.out.join(info.key());
    if let Some(marker) = finished(&dir) {
        say("Already downloaded");
        return Ok(marker);
    }
    fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;

    say("Downloading");
    let mut tracker = ProgressTracker::default();
    tool.download(&info.url_or(&target), &dir, cancel, &mut |line| {
        if let Some((fraction, text)) = tracker.update(line) {
            emit(&Event::Progress { fraction: Some(fraction), text });
        }
    })?;
    let file = ytdlp::downloaded_file(&dir).context("yt-dlp finished, but there is no video in its folder")?;

    let mut meta = BTreeMap::from([("link".to_owned(), link.to_owned()), ("video".to_owned(), info.url_or(&target))]);
    if let Some(title) = &song.title {
        meta.insert("title".to_owned(), title.clone());
    }
    if let Some(artist) = &song.artist {
        meta.insert("artist".to_owned(), artist.clone());
    }
    let marker = Marker { file, name: display_name(&song, &info.title), meta };
    write_marker(&dir, &marker)?;
    Ok(marker)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--describe") => {
            println!("{DESCRIPTION}");
            ExitCode::SUCCESS
        }
        Some("fetch") => {
            let parsed = match parse_fetch_args(&args[1..]) {
                Ok(parsed) => parsed,
                Err(error) => {
                    emit(&Event::Error { message: format!("{error:#}") });
                    return ExitCode::from(2);
                }
            };
            let cancel = Cancel::default();
            let on_close = cancel.clone();
            std::thread::spawn(move || {
                // The editor closes this to cancel. It also closes if the editor itself goes away.
                let _ = std::io::stdin().lock().read_to_end(&mut Vec::new());
                on_close.trigger();
            });
            match fetch(&parsed, &cancel) {
                Ok(marker) => {
                    emit(&Event::Done { path: &marker.file, name: &marker.name, meta: &marker.meta });
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    emit(&Event::Error { message: format!("{error:#}") });
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("Usage: simple_editor-link --describe");
            eprintln!("       simple_editor-link fetch --input <link> --out <folder> [--option key=value]...");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn the_description_is_valid_json() {
        let description: serde_json::Value = serde_json::from_str(DESCRIPTION).unwrap();
        assert_eq!(description["protocol"], 1);
        assert_eq!(description["kind"], "source");
        assert_eq!(description["options"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn reads_its_arguments() {
        let args = parse_fetch_args(&strings(&["--input", "http://a", "--out", "d", "--option", "cookies=c=d.txt", "--option", "max_height=720"])).unwrap();
        assert_eq!(args.input, "http://a");
        assert_eq!(args.out, PathBuf::from("d"));
        assert_eq!(args.options["cookies"], "c=d.txt");
        assert_eq!(args.options["max_height"], "720");

        assert!(parse_fetch_args(&strings(&["--out", "d"])).is_err());
        assert!(parse_fetch_args(&strings(&["--input"])).is_err());
        assert!(parse_fetch_args(&strings(&["--input", "a", "--out", "d", "--option", "novalue"])).is_err());
        assert!(parse_fetch_args(&strings(&["--what"])).is_err());
    }

    #[test]
    fn only_web_links_are_accepted() {
        assert_eq!(check_link("  https://youtu.be/x \n").unwrap(), "https://youtu.be/x");
        assert!(check_link("--exec=calc").is_err());
        assert!(check_link("file:///c:/x").is_err());
        assert!(check_link("ytsearch:anything").is_err());
    }

    #[test]
    fn events_are_what_the_editor_expects() {
        let line = serde_json::to_string(&Event::Progress { fraction: Some(0.5), text: "x" }).unwrap();
        assert_eq!(line, r#"{"event":"progress","fraction":0.5,"text":"x"}"#);
        let meta = BTreeMap::new();
        let line = serde_json::to_string(&Event::Done { path: Path::new("a"), name: "n", meta: &meta }).unwrap();
        assert_eq!(line, r#"{"event":"done","path":"a","name":"n","meta":{}}"#);
        let line = serde_json::to_string(&Event::Error { message: "m".into() }).unwrap();
        assert_eq!(line, r#"{"event":"error","message":"m"}"#);
    }

    #[test]
    fn a_download_counts_only_when_its_marker_and_file_are_there() {
        let dir = std::env::temp_dir().join(format!("simple_editor_link_marker_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("video.mp4");
        let marker = Marker { file: file.clone(), name: "n".into(), meta: BTreeMap::new() };

        assert_eq!(finished(&dir), None);
        write_marker(&dir, &marker).unwrap();
        assert_eq!(finished(&dir), None, "the file is not there yet");
        fs::write(&file, "video").unwrap();
        assert_eq!(finished(&dir), Some(marker));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_name_prefers_what_song_link_said() {
        let song = songlink::Song { title: Some("T".into()), artist: Some("A".into()), youtube: None };
        assert_eq!(display_name(&song, "video"), "T by A");
        let song = songlink::Song { title: Some("T".into()), artist: None, youtube: None };
        assert_eq!(display_name(&song, "video"), "T");
        assert_eq!(display_name(&songlink::Song::default(), "video"), "video");
    }
}
