//! ds-tool: changes DicingSplicing projects from the command line, so that scripts and AI agents can
//! lay recordings out without the editor's window. Every change goes through the editor's own model,
//! keeps its rules and is saved the way the editor saves. docs/ds-tool.md tells how to use it.

mod args;
mod pieces;
mod report;
mod store;

use crate::core::edit::Clip;
use crate::core::lock;
use crate::core::preset::PresetId;
use crate::core::project::{file_label, Project};
use crate::core::settings::{self, Settings};
use crate::media::probe::{self, MediaInfo};
use crate::media::timecode::format;
use anyhow::{bail, Context, Result};
use args::{wrong, Args};
use pieces::Piece;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Once;

/// What a command answers: its text, and whether it did what was asked.
struct Answer {
    text: String,
    ok: bool,
}

/// Runs ds-tool with the words after its name, and returns its exit code: 0 when it did what was
/// asked, 1 when it could not, or `check` found something wrong.
pub fn run(args: Vec<String>) -> i32 {
    let Some((command, rest)) = args.split_first() else {
        print!("{}", help(None));
        return 0;
    };
    if matches!(command.as_str(), "help" | "--help" | "-h") {
        print!("{}", help(rest.first().map(String::as_str)));
        return 0;
    }
    match execute(command, rest, &read_file) {
        Ok(answer) => {
            println!("{}", answer.text);
            if answer.ok {
                0
            } else {
                1
            }
        }
        Err(error) => {
            eprintln!("ds-tool {command}: {error:#}.");
            // How it is written, when that is what was wrong.
            if let Some(usage) = usage(command).filter(|_| error.downcast_ref::<args::Wrong>().is_some()) {
                eprintln!("Usage: {usage}\nMore: ds-tool help {command}");
            }
            1
        }
    }
}

/// What ffprobe finds in a file, with ffprobe looked for as the editor looks for it.
fn read_file(path: &Path) -> Result<MediaInfo> {
    static PROGRAMS: Once = Once::new();
    PROGRAMS.call_once(|| Settings::load(&settings::default_path()).0.apply_programs());
    probe::probe(path).with_context(|| format!("can't read {}", path.display()))
}

/// Runs `command` with `args`, reading files with `read`.
fn execute(command: &str, args: &[String], read: &dyn Fn(&Path) -> Result<MediaInfo>) -> Result<Answer> {
    let line = std::iter::once(command.to_owned()).chain(args.iter().map(|arg| quoted(arg))).collect::<Vec<_>>().join(" ");
    match command {
        "new" => {
            let args = Args::parse(args, &["name", "frame"], &[])?;
            let [folder] = args.words(["<folder>"])?;
            let dir = absolute(folder);
            let name = args.option("name").map_or_else(|| file_label(&dir), str::to_owned);
            let frame = args.option("frame").map(pieces::frame_named).transpose()?.unwrap_or(PresetId::Landscape);
            let project = pieces::new_project(&name, frame);
            store::create(&dir, &project)?;
            answer(&args, format!("Made the project {name} in {}. Append puts clips in it.", dir.display()), &project)
        }
        "build" => {
            let args = Args::parse(args, &[], &[])?;
            let [folder, list] = args.words(["<folder>", "<list.json>"])?;
            let (dir, list) = (absolute(folder), absolute(list));
            let text = std::fs::read_to_string(&list).with_context(|| format!("can't read {}", list.display()))?;
            let wanted: List = serde_json::from_str(&text).with_context(|| format!("{} is not a list of pieces", list.display()))?;
            if wanted.pieces.is_empty() {
                bail!("{} has no pieces", list.display());
            }
            let name = wanted.name.unwrap_or_else(|| file_label(&dir));
            let frame = wanted.frame.as_deref().map(pieces::frame_named).transpose()?.unwrap_or(PresetId::Landscape);
            let mut project = pieces::new_project(&name, frame);
            let base = list.parent().map_or_else(PathBuf::new, Path::to_owned);
            for (i, item) in wanted.pieces.iter().enumerate() {
                let mut put = || -> Result<()> {
                    let piece = Piece { file: base.join(&item.file), from: item.from.as_ref().map(Time::seconds).transpose()?, to: item.to.as_ref().map(Time::seconds).transpose()?, name: item.name.clone(), fit: item.fit };
                    let info = read_piece_file(&piece.file, read)?;
                    pieces::put(&mut project, &piece, &info, None)?;
                    Ok(())
                };
                put().with_context(|| format!("piece {}", i + 1))?;
            }
            store::create(&dir, &project)?;
            let count = project.sequence.clips().count();
            answer(&args, format!("Made the project {name} in {} from {}: {}.", dir.display(), list.display(), pieces::clips(count)), &project)
        }
        "append" | "insert" => {
            let allowed: &[&str] = if command == "append" { &["from", "to", "name"] } else { &["before", "from", "to", "name"] };
            let args = Args::parse(args, allowed, &["fit"])?;
            let [project, file] = args.words(["<project>", "<file>"])?;
            let before = match command {
                "insert" => Some(args.number("before")?.ok_or_else(|| wrong("it needs --before N: the clip to put it before"))?),
                _ => None,
            };
            let piece = Piece { file: absolute(file), from: args.time("from")?, to: args.time("to")?, name: args.option("name").map(str::to_owned), fit: args.flag("fit") };
            let info = read_piece_file(&piece.file, read)?;
            let (done, project) = store::change(&store::folder_of(project), &line, |project| {
                let number = pieces::put(project, &piece, &info, before)?;
                let clips = video_clips(project);
                let clip = &clips[number - 1];
                let moved = moved_note(clips.len() - number, clip.span().len());
                let bars = if clip.transform.bars && clip.transform.zoom < 1.0 - pieces::ROUNDING { ", all of its picture with black bars" } else { "" };
                Ok(format!("Put clip {number}, {}: {} from {} to {}, {} long{bars}{moved}.", clip.name, file_label(&piece.file), format(clip.source_in), format(clip.source_out), format(clip.span().len())))
            })?;
            answer(&args, done, &project)
        }
        "delete" => {
            let args = Args::parse(args, &[], &[])?;
            let [project, number] = args.words(["<project>", "N"])?;
            let number = args::parse_number(number)?;
            let (done, project) = store::change(&store::folder_of(project), &line, |project| {
                let clip = pieces::delete(project, number)?;
                let moved = moved_note(video_clips(project).len() + 1 - number, -clip.span().len());
                Ok(format!("Deleted clip {number}, {}{moved}.", clip.name))
            })?;
            answer(&args, done, &project)
        }
        "move" => {
            let args = Args::parse(args, &["to"], &[])?;
            let [project, number] = args.words(["<project>", "N"])?;
            let number = args::parse_number(number)?;
            let to = args.number("to")?.ok_or_else(|| wrong("it needs --to M: the place to move the clip to"))?;
            let (done, project) = store::change(&store::folder_of(project), &line, |project| {
                let clip = pieces::reorder(project, number, to)?;
                Ok(format!("Moved clip {number}, {}, to place {to}.", clip.name))
            })?;
            answer(&args, done, &project)
        }
        "trim" => {
            let args = Args::parse(args, &["from", "to"], &[])?;
            let [project, number] = args.words(["<project>", "N"])?;
            let number = args::parse_number(number)?;
            let (from, to) = (args.time("from")?, args.time("to")?);
            if from.is_none() && to.is_none() {
                return Err(wrong("it needs --from, --to or both: where in its file the clip is to start and end"));
            }
            let (done, project) = store::change(&store::folder_of(project), &line, |project| {
                let (before, after) = pieces::retime(project, number, from, to)?;
                let moved = moved_note(video_clips(project).len() - number, after.span().len() - before.span().len());
                Ok(format!("Clip {number}, {}, plays from {} to {} now, {} long{moved}.", after.name, format(after.source_in), format(after.source_out), format(after.span().len())))
            })?;
            answer(&args, done, &project)
        }
        "blade" => {
            let args = Args::parse(args, &["at"], &[])?;
            let [project, number] = args.words(["<project>", "N"])?;
            let number = args::parse_number(number)?;
            let at = args.time("at")?.ok_or_else(|| wrong("it needs --at TIME: where in its recording to cut the clip"))?;
            let (done, project) = store::change(&store::folder_of(project), &line, |project| {
                let clip = pieces::blade(project, number, at)?;
                let (next, from, to, at) = (number + 1, format(clip.source_in), format(clip.source_out), format(at));
                Ok(format!("Cut clip {number}, {}, at {at} of its recording: clip {number} plays it from {from} to {at}, clip {next} from {at} to {to}.", clip.name))
            })?;
            answer(&args, done, &project)
        }
        "undo" => {
            let args = Args::parse(args, &[], &[])?;
            let [project] = args.words(["<project>"])?;
            let (done, project) = store::undo(&store::folder_of(project))?;
            answer(&args, done, &project)
        }
        "show" => {
            let args = Args::parse(args, &[], &[])?;
            let [project] = args.words(["<project>"])?;
            let dir = store::folder_of(project);
            let project = store::load(&dir)?;
            let note = if lock::held(&dir) { "The editor has the project open: what it shows may be newer than what is saved." } else { "" };
            answer(&args, note.to_owned(), &project)
        }
        "check" => {
            let args = Args::parse(args, &[], &[])?;
            let [project] = args.words(["<project>"])?;
            let project = store::load(&store::folder_of(project))?;
            let problems = report::problems(&project, read);
            let summary = report::summary(&project);
            let text = if args.json {
                serde_json::to_string_pretty(&serde_json::json!({"problems": problems, "project": summary.json()}))?
            } else if problems.is_empty() {
                format!("No problems.\n\n{}", summary.text())
            } else {
                format!("Problems:\n{}\n\n{}", problems.iter().map(|problem| format!("- {problem}.")).collect::<Vec<_>>().join("\n"), summary.text())
            };
            Ok(Answer { text, ok: problems.is_empty() })
        }
        other => bail!("there is no command {other:?}: ds-tool help lists them"),
    }
}

/// The clips of Video 1.
fn video_clips(project: &Project) -> &[Clip] {
    pieces::find_video_track(&project.sequence).map_or(&[], |track| &project.sequence.tracks[track].clips)
}

/// What happened to the `after` clips after a changed one, which moved them by `delta` seconds.
fn moved_note(after: usize, delta: f64) -> String {
    if after == 0 || delta.abs() <= pieces::ROUNDING {
        return String::new();
    }
    let way = if delta > 0.0 { "on" } else { "back" };
    format!("; the {} after it moved {way} by {}", if after == 1 { "clip" } else { "clips" }, format(delta.abs()))
}

/// The answer of a command that did `done` to `project`, or only looked at it with nothing done.
fn answer(args: &Args, done: String, project: &Project) -> Result<Answer> {
    let summary = report::summary(project);
    let text = if args.json {
        let mut answer = serde_json::json!({"project": summary.json()});
        if !done.is_empty() {
            answer["done"] = done.into();
        }
        serde_json::to_string_pretty(&answer)?
    } else if done.is_empty() {
        summary.text()
    } else {
        format!("{done}\n\n{}", summary.text())
    };
    Ok(Answer { text, ok: true })
}

/// What a piece's file holds, or why it can't be put in.
fn read_piece_file(file: &Path, read: &dyn Fn(&Path) -> Result<MediaInfo>) -> Result<MediaInfo> {
    if !file.is_file() {
        bail!("there is no file {}", file.display());
    }
    read(file)
}

fn absolute(path: &str) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path))
}

/// An argument as it would be typed: in quotes when it has spaces.
fn quoted(arg: &str) -> String {
    if arg.contains(char::is_whitespace) {
        format!("\"{arg}\"")
    } else {
        arg.to_owned()
    }
}

/// What `build` reads: the project's name and frame, and its pieces in order.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    name: Option<String>,
    frame: Option<String>,
    pieces: Vec<ListPiece>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListPiece {
    /// The recording, by its full path or the path from the list's folder.
    file: PathBuf,
    from: Option<Time>,
    to: Option<Time>,
    name: Option<String>,
    /// All of the picture in the frame, as --fit.
    #[serde(default)]
    fit: bool,
}

/// A time in a list: a number of seconds, or text as on the command line.
#[derive(Deserialize)]
#[serde(untagged)]
enum Time {
    Seconds(f64),
    Text(String),
}

impl Time {
    fn seconds(&self) -> Result<f64> {
        match self {
            Time::Seconds(seconds) if seconds.is_finite() && *seconds >= 0.0 => Ok(*seconds),
            Time::Seconds(seconds) => bail!("{seconds} is not a time"),
            Time::Text(text) => args::parse_time(text),
        }
    }
}

/// How a command is written.
fn usage(command: &str) -> Option<&'static str> {
    Some(match command {
        "new" => "ds-tool new <folder> [--name NAME] [--frame FRAME]",
        "build" => "ds-tool build <folder> <list.json>",
        "append" => "ds-tool append <project> <file> [--from TIME] [--to TIME] [--name NAME] [--fit]",
        "insert" => "ds-tool insert <project> <file> --before N [--from TIME] [--to TIME] [--name NAME] [--fit]",
        "delete" => "ds-tool delete <project> N",
        "move" => "ds-tool move <project> N --to M",
        "trim" => "ds-tool trim <project> N [--from TIME] [--to TIME]",
        "blade" => "ds-tool blade <project> N --at TIME",
        "show" => "ds-tool show <project>",
        "check" => "ds-tool check <project>",
        "undo" => "ds-tool undo <project>",
        _ => return None,
    })
}

const COMMANDS: [&str; 11] = ["new", "build", "append", "insert", "delete", "move", "trim", "blade", "show", "check", "undo"];
/// The whole guide to ds-tool: next to the program in the editor's package, in docs in its source.
const GUIDE: &str = "ds-tool.md";

/// Where the whole guide is: its path when it is next to this program, as in the editor's package.
fn guide() -> String {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|folder| folder.join(GUIDE))).filter(|guide| guide.is_file());
    match beside {
        Some(guide) => format!("The whole guide, with an example, the rules and a request to give an agent: {}", guide.display()),
        None => format!("The whole guide, with an example, the rules and a request to give an agent: {GUIDE}, next to ds-tool.exe in the editor's package, or docs/{GUIDE} in its source."),
    }
}

/// What `help` prints: about all the commands, or about one.
fn help(command: Option<&str>) -> String {
    let Some(command) = command else {
        let usages: Vec<&str> = COMMANDS.iter().filter_map(|command| usage(command)).collect();
        return format!(
            "ds-tool changes DicingSplicing projects from the command line, for scripts and AI agents. It lays
pieces of recordings out on Video 1, one after another, keeping the editor's rules.

  {}

<project> is a project's folder, or its project.json. N and M are clip numbers, as show lists
them, 1 for the first. TIME is seconds (75.5), minutes and seconds (1:15.5), or hours too (1:02:03).
--json after any command answers in JSON. ds-tool help <command> tells more about one.

While the editor has a project open, ds-tool does not change it: the editor saves what it shows by
itself, and would write over the change. Close the project in the editor first.

{}
",
            usages.join("\n  "),
            guide()
        );
    };
    let about = match command {
        "new" => "Makes a project with nothing in it yet, in <folder>, which it makes if it is not there.
--name is the project's name; without it, the folder's. --frame is the frame the video is made
for: Landscape (16:9, the default), Vertical (9:16), Square, Tablet (16:10), Photo (3:2),
Classic (4:3), Post (4:5), Cinema (21:9) or TelegramCircle. The export settings are the editor's
preset for that frame, or its YouTube preset.",
        "build" => "Makes a project in <folder> from a list, at once: the same as new and an append for each piece,
and when one piece can't go in, nothing is made. The list is JSON:

  {
    \"name\": \"Lesson 3\",
    \"frame\": \"Landscape\",
    \"pieces\": [
      { \"file\": \"rec/01_intro.mp4\", \"name\": \"Intro\" },
      { \"file\": \"rec/02_install.mp4\", \"from\": \"0:12\", \"to\": \"4:05\", \"name\": \"Installing\", \"fit\": true }
    ]
  }

A file's path is full, or goes from the list's folder. from and to are TIME, as text or seconds;
fit is --fit.",
        "append" => "Puts a piece of a recording at the end of Video 1, right after the last clip. Without --from it
starts where the recording starts, without --to it ends where the recording ends. --name is what
the clip is called on the timeline; without it, the file's name. Its sound comes with it.
--fit shows all of the picture: a recording of other proportions than the frame, such as a 16:10
screen in the 16:9 frame, gets black bars instead of losing its edges, as Fit does in the editor.",
        "insert" => "Puts a piece of a recording before clip N of Video 1. Clip N and the clips after it move on
by the piece's length. The options are as for append.",
        "delete" => "Takes clip N off Video 1. The clips after it move back by its length: no gap is left. The
recording stays among the project's sources.",
        "move" => "Moves clip N to place M: it becomes clip M, and the clips stay one after another.",
        "trim" => "Makes clip N play another part of its recording: --from and --to are times in the recording,
and the one not given stays. The clip keeps its start; the clips after it move by as much as it
got longer or shorter.",
        "blade" => "Cuts clip N in two where its recording is at TIME, as the Blade does in the editor: clip N keeps
what is before the cut, and a new clip N+1 right after it plays the rest, with the same name,
framing and sound. Nothing moves. To take a part out of the middle of a recording, cut at both of
its ends and delete the clip between: blade N --at 1:00, blade N+1 --at 1:30, delete N+1.",
        "show" => "Tells what the project has: its frame, the clips of Video 1 in order, with where each starts on
the timeline, how long it is and what part of which file it plays, the other tracks, the export
range, and what may not be meant, such as a gap between clips.",
        "check" => "Looks at the files too: whether every recording is there and can be read, and whether the clips
play only what the recordings have. Says what is wrong; it answers 1 when something is.",
        "undo" => "Takes back the last change ds-tool made to the project, if nothing has changed the project
since. Only the last one: a second undo has nothing to take back.",
        other => return format!("There is no command {other:?}.\n\n{}", help(None)),
    };
    format!("{}\n\n{about}\n", usage(command).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::pieces::tests::video;
    use std::fs;

    /// Runs a command as from the command line, on files that `video` says are 60 s long.
    fn ds(command: &str, args: &[&str]) -> Result<Answer> {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        execute(command, &args, &|_| Ok(video(60.0)))
    }

    #[test]
    fn a_tutorial_laid_out_command_by_command() {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_cli_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for file in ["intro.mp4", "install.mp4", "result.mp4"] {
            fs::write(dir.join(file), "").unwrap();
        }
        let lesson = dir.join("lesson");
        let (lesson, file) = (lesson.to_str().unwrap(), |name: &str| dir.join(name).to_string_lossy().into_owned());
        let made = ds("new", &[lesson, "--name", "Lesson 3"]).unwrap().text;
        assert!(made.starts_with("Made the project Lesson 3 in "));
        assert!(made.ends_with("Lesson 3: frame Landscape (16:9), Video 1 has no clips, 00:00.00 long.\nExport range: 00:00.00 to 00:00.00."));
        ds("append", &[lesson, &file("intro.mp4"), "--to", "0:12.5", "--name", "Intro"]).unwrap();
        ds("append", &[lesson, &file("result.mp4"), "--from", "30"]).unwrap();
        let put = ds("insert", &[lesson, &file("install.mp4"), "--before", "2", "--from", "0:05", "--to", "0:50"]).unwrap().text;
        assert!(put.starts_with("Put clip 2, install.mp4: install.mp4 from 00:05.00 to 00:50.00, 00:45.00 long; the clip after it moved on by 00:45.00."), "{put}");
        let moved = ds("move", &[lesson, "3", "--to", "1"]).unwrap().text;
        assert!(moved.starts_with("Moved clip 3, result.mp4, to place 1."));
        let trimmed = ds("trim", &[lesson, "3", "--to", "40"]).unwrap().text;
        assert!(trimmed.starts_with("Clip 3, install.mp4, plays from 00:05.00 to 00:40.00 now, 00:35.00 long.\n"), "the last clip: nothing after it to move. {trimmed}");
        let trimmed = ds("trim", &[lesson, "2", "--from", "2.5"]).unwrap().text;
        assert!(trimmed.starts_with("Clip 2, Intro, plays from 00:02.50 to 00:12.50 now, 00:10.00 long; the clip after it moved back by 00:02.50."), "{trimmed}");
        let deleted = ds("delete", &[lesson, "1"]).unwrap().text;
        assert!(deleted.starts_with("Deleted clip 1, result.mp4; the clips after it moved back by 00:30.00."));
        let shown = ds("show", &[lesson, "--json"]).unwrap().text;
        let json: serde_json::Value = serde_json::from_str(&shown).unwrap();
        let clips: Vec<(String, f64, f64)> = json["project"]["clips"].as_array().unwrap().iter().map(|clip| (clip["name"].as_str().unwrap().to_owned(), clip["start"].as_f64().unwrap(), clip["length"].as_f64().unwrap())).collect();
        assert_eq!(clips, [("Intro".to_owned(), 0.0, 10.0), ("install.mp4".to_owned(), 10.0, 35.0)]);
        let undone = ds("undo", &[lesson]).unwrap().text;
        assert!(undone.starts_with(&format!("Took back `delete {lesson} 1`.")), "{undone}");
        let checked = ds("check", &[lesson]).unwrap();
        assert!(checked.ok && checked.text.starts_with("No problems."));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn blade_cuts_where_the_recording_is_at() {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_blade_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("talk.mp4"), "").unwrap();
        let (lesson, talk) = (dir.join("lesson"), dir.join("talk.mp4"));
        let (lesson, talk) = (lesson.to_str().unwrap(), talk.to_str().unwrap());
        ds("new", &[lesson]).unwrap();
        ds("append", &[lesson, talk, "--from", "10"]).unwrap();
        let cut = ds("blade", &[lesson, "1", "--at", "0:25"]).unwrap().text;
        assert!(cut.starts_with("Cut clip 1, talk.mp4, at 00:25.00 of its recording: clip 1 plays it from 00:10.00 to 00:25.00, clip 2 from 00:25.00 to 01:00.00."), "{cut}");
        assert!(cut.contains("Video 1 has 2 clips, 00:50.00 long."), "{cut}");
        assert_eq!(ds("blade", &[lesson, "1"]).err().unwrap().to_string(), "it needs --at TIME: where in its recording to cut the clip");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_whole_project_from_a_list_or_nothing() {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_build_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("rec")).unwrap();
        fs::write(dir.join("rec/a.mp4"), "").unwrap();
        let list = dir.join("list.json");
        let write_list = |pieces: &str| fs::write(&list, format!("{{\"name\": \"From a list\", \"frame\": \"Vertical\", \"pieces\": [{pieces}]}}")).unwrap();
        let (lesson, list_path) = (dir.join("lesson"), list.to_string_lossy().into_owned());
        write_list(r#"{"file": "rec/a.mp4", "to": "0:20"}, {"file": "rec/b.mp4"}"#);
        let error = ds("build", &[lesson.to_str().unwrap(), &list_path]).err().unwrap();
        assert_eq!(format!("{error:#}"), format!("piece 2: there is no file {}", dir.join("rec/b.mp4").display()));
        assert!(!lesson.exists(), "nothing was made");
        write_list(r#"{"file": "rec/a.mp4", "to": "0:20", "name": "First"}, {"file": "rec/a.mp4", "from": 40}"#);
        let made = ds("build", &[lesson.to_str().unwrap(), &list_path]).unwrap().text;
        assert!(made.contains(": 2 clips.\n\nFrom a list: frame Vertical (9:16), Video 1 has 2 clips, 00:40.00 long."), "{made}");
        write_list(r#"{"file": "rec/a.mp4", "form": 3}"#);
        let error = ds("build", &[dir.join("other").to_str().unwrap(), &list_path]).err().unwrap();
        assert!(format!("{error:#}").contains("unknown field `form`"), "{error:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn help_tells_every_command() {
        let all = help(None);
        for command in COMMANDS {
            assert!(all.contains(usage(command).unwrap()), "{command}");
            assert!(help(Some(command)).starts_with(usage(command).unwrap()));
        }
        assert!(help(Some("remove")).starts_with("There is no command \"remove\"."));
        assert!(format!("{:#}", ds("remove", &[]).err().unwrap()).contains("ds-tool help lists them"));
        let wrong = ds("append", &["lesson"]).err().unwrap().to_string();
        assert_eq!(wrong, "it takes <project> <file>, and got 1 word");
    }
}
