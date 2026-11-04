//! A plugin for Simple Editor: sends a finished video to a Telegram channel, as a circle.
//!
//! It is a program on its own and knows nothing of the editor except the plugin protocol, described
//! in the editor's `src/core/plugins.rs`. The editor knows nothing of the bot, the token or the
//! channels: those are in `credentials.json`, which only this program reads. To try it by hand:
//!
//!     simple_editor-telegram prepare --file <video> --data <folder> [--name <project>] [--meta key=value]...
//!     simple_editor-telegram send --file <video> --data <folder> --field channel=test [--field text=...] [--field link=...]
//!
//! It answers with one JSON object per line. Closing its standard input cancels the sending.

mod api;
mod caption;
mod credentials;
mod journal;
mod video;

use anyhow::{bail, Context, Result};
use api::{Api, Cancel, Note, Step};
use credentials::Credentials;
use journal::Journal;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use video::Video;

const DESCRIPTION: &str = r#"{
  "protocol": 1,
  "kind": "publish",
  "name": "Send to Telegram",
  "extensions": ["mp4"],
  "options": [
    { "key": "credentials", "label": "Credentials file", "kind": "file", "hint": "credentials.json near the program" }
  ]
}"#;

const CREDENTIALS_OPTION: &str = "credentials";
const TEXT_FIELD: &str = "text";
const LINK_FIELD: &str = "link";
const CHANNEL_FIELD: &str = "channel";

#[derive(Debug, Default, PartialEq, Serialize)]
struct Choice {
    value: String,
    label: String,
    action: String,
    warnings: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
struct Field {
    key: &'static str,
    label: &'static str,
    kind: &'static str,
    value: String,
    hint: &'static str,
    choices: Vec<Choice>,
}

/// What the editor shows before sending.
#[derive(Debug, Default, PartialEq, Serialize)]
struct Form {
    summary: String,
    fields: Vec<Field>,
    problems: Vec<String>,
    warnings: Vec<String>,
}

/// What this program says while it works.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
enum Event<'a> {
    Progress { fraction: Option<f32>, text: &'a str },
    Form(&'a Form),
    Done { message: &'a str },
    Error { message: &'a str },
}

fn emit(event: &Event) {
    if let Ok(line) = serde_json::to_string(event) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

#[derive(Debug, PartialEq)]
struct Args {
    file: PathBuf,
    /// This program's own folder, for the journal.
    data: PathBuf,
    name: String,
    meta: BTreeMap<String, String>,
    fields: BTreeMap<String, String>,
    options: BTreeMap<String, String>,
}

fn parse_args(args: &[String]) -> Result<Args> {
    let (mut file, mut data, mut name) = (None, None, String::new());
    let (mut meta, mut fields, mut options) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let value = args.next().with_context(|| format!("{arg} needs a value"))?;
        let insert = |map: &mut BTreeMap<String, String>| -> Result<()> {
            let (key, value) = value.split_once('=').with_context(|| format!("{arg} {value}: it should be key=value"))?;
            map.insert(key.to_owned(), value.to_owned());
            Ok(())
        };
        match arg.as_str() {
            "--file" => file = Some(PathBuf::from(value)),
            "--data" => data = Some(PathBuf::from(value)),
            "--name" => name.clone_from(value),
            "--meta" => insert(&mut meta)?,
            "--field" => insert(&mut fields)?,
            "--option" => insert(&mut options)?,
            other => bail!("I don't know {other}"),
        }
    }
    Ok(Args { file: file.context("--file is missing")?, data: data.context("--data is missing")?, name, meta, fields, options })
}

impl Args {
    fn option(&self, key: &str) -> Option<&str> {
        self.options.get(key).map(|value| value.trim()).filter(|value| !value.is_empty())
    }

    fn field(&self, key: &str) -> &str {
        self.fields.get(key).map_or("", String::as_str)
    }

    fn file_name(&self) -> String {
        self.file.file_name().map_or_else(|| self.file.display().to_string(), |name| name.to_string_lossy().into_owned())
    }

    fn credentials(&self) -> Result<Credentials> {
        Credentials::load(self.option(CREDENTIALS_OPTION))
    }
}

fn ffprobe() -> String {
    std::env::var("SIMPLE_EDITOR_FFPROBE").ok().filter(|path| !path.is_empty()).unwrap_or_else(|| "ffprobe".to_owned())
}

fn channel_field(credentials: &Credentials, journal: &Journal, key: Option<&str>, now: u64) -> Field {
    let channels = credentials.channels();
    let warnings = |id: &str, label: &str| key.and_then(|key| journal.warning(key, id, label, now)).into_iter().collect();
    let choices = channels
        .iter()
        .map(|channel| Choice {
            value: channel.id.to_owned(),
            // The chat itself, to see where this will really go.
            label: format!("{} ({})", channel.label, channel.chat),
            action: format!("Send to {}", channel.label),
            warnings: warnings(channel.id, channel.label),
        })
        .collect();
    // The test channel is the first of them, when there is one.
    let value = channels.first().map_or_else(String::new, |channel| channel.id.to_owned());
    Field { key: CHANNEL_FIELD, label: "Channel", kind: "choice", value, hint: "", choices }
}

/// The form, from what could be found out. What couldn't be is a reason not to send, and is said.
fn build_form(args: &Args, credentials: Result<Credentials>, video: Result<Video>, journal: &Journal, now: u64) -> Form {
    let mut form = Form { summary: args.file_name(), ..Form::default() };
    match &video {
        Ok(video) => {
            form.summary = video.summary(&args.file_name());
            form.problems.extend(video.problems());
            form.warnings.extend(video.warnings());
        }
        Err(error) => form.problems.push(format!("{error:#}")),
    }
    form.fields.push(Field {
        key: TEXT_FIELD,
        label: "Text",
        kind: "text",
        value: caption::default_text(&args.meta, &args.name),
        hint: "Empty, with an empty link: only the circle goes",
        choices: Vec::new(),
    });
    form.fields.push(Field {
        key: LINK_FIELD,
        label: "Link",
        kind: "text",
        value: caption::default_link(&args.meta),
        hint: "The text will lead here",
        choices: Vec::new(),
    });
    match &credentials {
        Ok(credentials) => {
            let key = journal::file_key(&args.file).ok();
            form.fields.push(channel_field(credentials, journal, key.as_deref(), now));
        }
        Err(error) => form.problems.push(format!("{error:#}")),
    }
    form
}

fn prepare(args: &Args) {
    let video = Video::inspect(&args.file, &ffprobe());
    let form = build_form(args, args.credentials(), video, &Journal::load(&args.data), journal::now());
    emit(&Event::Form(&form));
}

/// What the user is told in the end.
fn outcome_text(label: &str, delivered: &api::Delivered) -> String {
    let mut text = format!("Sent to {label}");
    if !delivered.uploaded {
        text.push_str(" without uploading it again");
    }
    if !delivered.round {
        text.push_str(". Telegram shows it as an ordinary video, not a circle");
    }
    text
}

fn send(args: &Args, credentials: &Credentials, ffprobe: &str, cancel: &Cancel, say: &mut dyn FnMut(Option<f32>, &str)) -> Result<String> {
    // Asked for by name: going to the main channel is never what happens by default.
    let channel = match args.field(CHANNEL_FIELD) {
        "" => bail!("No channel was chosen"),
        id => credentials.channel(id)?,
    };
    // What the form said may be old by now: the file could have been exported again.
    say(None, "Looking at the file");
    let video = Video::inspect(&args.file, ffprobe)?;
    let problems = video.problems();
    if !problems.is_empty() {
        bail!("{}", problems.join(" "));
    }
    // Before anything goes out: a bad link should not leave half a post.
    let caption = caption::message(args.field(TEXT_FIELD), args.field(LINK_FIELD))?;

    let key = journal::file_key(&args.file)?;
    let mut journal = Journal::load(&args.data);
    let api = Api::new(credentials.api_url(), credentials.token());
    let note = Note { chat: &channel.chat, length: video.width, duration: video.whole_seconds() };
    let delivered = api::deliver(&api, &note, caption.as_deref(), &args.file, journal.file_id(&key), cancel, &mut |step| match step {
        Step::Message => say(None, "Sending the message"),
        Step::Resending => say(None, "Sending the circle"),
        Step::Uploading(fraction) if fraction < 1.0 => say(Some(fraction), "Uploading"),
        Step::Uploading(_) => say(Some(1.0), "Waiting for Telegram"),
    })?;

    let mut text = outcome_text(channel.label, &delivered);
    journal.record(&key, &args.file_name(), channel.id, delivered.file_id, journal::now());
    // It has been sent all the same.
    if let Err(error) = journal.save(&args.data) {
        text.push_str(&format!(". It couldn't be noted in the journal: {error:#}"));
    }
    Ok(text)
}

fn run_send(args: &Args, cancel: &Cancel) -> ExitCode {
    let credentials = match args.credentials() {
        Ok(credentials) => credentials,
        Err(error) => {
            emit(&Event::Error { message: &format!("{error:#}") });
            return ExitCode::FAILURE;
        }
    };
    let mut say = |fraction: Option<f32>, text: &str| emit(&Event::Progress { fraction, text });
    match send(args, &credentials, &ffprobe(), cancel, &mut say) {
        Ok(message) => {
            emit(&Event::Done { message: &message });
            ExitCode::SUCCESS
        }
        // Asked to stop, and nothing is left behind: there is nothing to report.
        Err(error) if error.is::<api::Stopped>() => ExitCode::FAILURE,
        Err(error) => {
            // The last place an error passes through: whatever it says, the token doesn't get out.
            emit(&Event::Error { message: &credentials.scrub(&format!("{error:#}")) });
            ExitCode::FAILURE
        }
    }
}

/// Calls `on_close` when the editor closes this program's input, or goes away.
fn watch_input(on_close: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        let _ = std::io::stdin().lock().read_to_end(&mut Vec::new());
        on_close();
    });
}

fn with_args(args: &[String], work: impl FnOnce(&Args) -> ExitCode) -> ExitCode {
    match parse_args(args) {
        Ok(parsed) => work(&parsed),
        Err(error) => {
            emit(&Event::Error { message: &format!("{error:#}") });
            ExitCode::from(2)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--describe") => {
            println!("{DESCRIPTION}");
            ExitCode::SUCCESS
        }
        Some("prepare") => with_args(&args[1..], |parsed| {
            // Only looking: nothing to take back.
            watch_input(|| std::process::exit(1));
            prepare(parsed);
            ExitCode::SUCCESS
        }),
        Some("send") => with_args(&args[1..], |parsed| {
            let cancel = Cancel::default();
            let on_close = cancel.clone();
            watch_input(move || on_close.trigger());
            run_send(parsed, &cancel)
        }),
        _ => {
            eprintln!("Usage: simple_editor-telegram --describe");
            eprintln!("       simple_editor-telegram prepare --file <video> --data <folder> [--name <project>] [--meta key=value]... [--option key=value]...");
            eprintln!("       simple_editor-telegram send --file <video> --data <folder> [--field key=value]... [--option key=value]...");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::path::Path;

    const TOKEN: &str = "123456:fake-Token_value";

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("simple_editor_telegram_main_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_file(dir: &Path) -> PathBuf {
        let file = dir.join("a.mp4");
        std::fs::write(&file, "video").unwrap();
        file
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    fn credentials(extra: &str) -> Credentials {
        Credentials::parse(format!(r#"{{"bot_token":"{TOKEN}","chat_id":"@main","chat_id_test":"@test"{extra}}}"#).as_bytes()).unwrap()
    }

    fn args(file: &Path, data: &Path, fields: &[(&str, &str)]) -> Args {
        Args {
            file: file.to_owned(),
            data: data.to_owned(),
            name: "Project".into(),
            meta: BTreeMap::new(),
            fields: fields.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect(),
            options: BTreeMap::new(),
        }
    }

    fn circle() -> Video {
        Video { width: 400, height: 400, duration: 30.0, bytes: 5 }
    }

    fn channel_of(form: &Form) -> &Field {
        form.fields.iter().find(|field| field.key == CHANNEL_FIELD).expect("a channel field")
    }

    #[test]
    fn the_description_is_valid_json() {
        let description: serde_json::Value = serde_json::from_str(DESCRIPTION).unwrap();
        assert_eq!(description["protocol"], 1);
        assert_eq!(description["kind"], "publish");
        assert_eq!(description["extensions"][0], "mp4");
        assert_eq!(description["options"][0]["key"], CREDENTIALS_OPTION);
    }

    #[test]
    fn reads_its_arguments() {
        let parsed = parse_args(&strings(&["--file", "a.mp4", "--data", "d", "--name", "N", "--meta", "title=a=b", "--field", "text=", "--option", "k=v"])).unwrap();
        assert_eq!(parsed.file, PathBuf::from("a.mp4"));
        assert_eq!(parsed.data, PathBuf::from("d"));
        assert_eq!(parsed.name, "N");
        assert_eq!(parsed.meta["title"], "a=b");
        assert_eq!(parsed.fields["text"], "");
        assert_eq!(parsed.options["k"], "v");

        assert!(parse_args(&strings(&["--data", "d"])).is_err());
        assert!(parse_args(&strings(&["--file", "a"])).is_err());
        assert!(parse_args(&strings(&["--file"])).is_err());
        assert!(parse_args(&strings(&["--file", "a", "--data", "d", "--field", "novalue"])).is_err());
        assert!(parse_args(&strings(&["--what", "x"])).is_err());
    }

    #[test]
    fn events_are_what_the_editor_expects() {
        let line = serde_json::to_string(&Event::Done { message: "m" }).unwrap();
        assert_eq!(line, r#"{"event":"done","message":"m"}"#);
        let form = Form { summary: "s".into(), ..Form::default() };
        let line = serde_json::to_string(&Event::Form(&form)).unwrap();
        assert_eq!(line, r#"{"event":"form","summary":"s","fields":[],"problems":[],"warnings":[]}"#);
    }

    #[test]
    fn the_test_channel_comes_first_and_the_button_names_the_channel() {
        let dir = test_dir("form");
        let file = test_file(&dir);
        let form = build_form(&args(&file, &dir, &[]), Ok(credentials("")), Ok(circle()), &Journal::default(), 0);
        assert!(form.problems.is_empty(), "{:?}", form.problems);
        let channel = channel_of(&form);
        assert_eq!(channel.value, "test");
        assert_eq!(channel.choices[0].action, "Send to Test");
        assert_eq!(channel.choices[0].label, "Test (@test)");
        assert_eq!(channel.choices[1].action, "Send to Main");
        assert_eq!(channel.choices[1].label, "Main (@main)");
    }

    #[test]
    fn with_only_a_main_channel_that_is_the_choice() {
        let dir = test_dir("only_main");
        let file = test_file(&dir);
        let only_main = Credentials::parse(format!(r#"{{"bot_token":"{TOKEN}","chat_id":"@main"}}"#).as_bytes()).unwrap();
        let form = build_form(&args(&file, &dir, &[]), Ok(only_main), Ok(circle()), &Journal::default(), 0);
        assert_eq!(channel_of(&form).value, "main");
        assert_eq!(channel_of(&form).choices.len(), 1);
    }

    #[test]
    fn the_form_starts_from_what_the_download_knew() {
        let dir = test_dir("meta");
        let file = test_file(&dir);
        let mut arguments = args(&file, &dir, &[]);
        arguments.meta = [("title", "T"), ("artist", "A"), ("link", "https://song.link/x")].map(|(k, v)| (k.to_owned(), v.to_owned())).into();
        let form = build_form(&arguments, Ok(credentials("")), Ok(circle()), &Journal::default(), 0);
        assert_eq!(form.fields[0].value, "\"T\" by A");
        assert_eq!(form.fields[1].value, "https://song.link/x");
    }

    #[test]
    fn what_stops_sending_is_said_in_the_form() {
        let dir = test_dir("problems");
        let file = test_file(&dir);
        let not_square = Video { width: 100, ..circle() };
        let form = build_form(&args(&file, &dir, &[]), Err(anyhow!("no credentials")), Ok(not_square), &Journal::default(), 0);
        assert_eq!(form.problems.len(), 2, "{:?}", form.problems);
        assert!(form.fields.iter().all(|field| field.key != CHANNEL_FIELD), "no channels are known");

        let form = build_form(&args(&file, &dir, &[]), Ok(credentials("")), Err(anyhow!("no ffprobe")), &Journal::default(), 0);
        assert_eq!(form.problems, ["no ffprobe"]);
        assert_eq!(form.summary, "a.mp4");
    }

    #[test]
    fn what_was_already_sent_is_warned_about_per_channel() {
        let dir = test_dir("sent_before");
        let file = test_file(&dir);
        let mut journal = Journal::default();
        journal.record(&journal::file_key(&file).unwrap(), "a.mp4", "test", None, 100);
        let form = build_form(&args(&file, &dir, &[]), Ok(credentials("")), Ok(circle()), &journal, 120);
        let channel = channel_of(&form);
        assert_eq!(channel.choices[0].warnings, ["This file already went to Test just now."]);
        assert!(channel.choices[1].warnings.is_empty());
        assert!(form.warnings.is_empty());
    }

    /// `send` with a stand-in for ffprobe: a script that says this is a good circle.
    #[cfg(windows)]
    fn fake_ffprobe(dir: &Path, width: u32) -> String {
        let path = dir.join("fake_ffprobe.cmd");
        std::fs::write(&path, format!("@echo {{\"streams\":[{{\"width\":{width},\"height\":400}}],\"format\":{{\"duration\":\"30.2\"}}}}\r\n")).unwrap();
        path.display().to_string()
    }

    #[cfg(windows)]
    fn sent(arguments: &Args, server: &api::fake::FakeTelegram, ffprobe: &str) -> Result<String> {
        let credentials = credentials(&format!(r#","api_url":"{}""#, server.url));
        send(arguments, &credentials, ffprobe, &Cancel::default(), &mut |_, _| {})
    }

    #[cfg(windows)]
    #[test]
    fn from_test_to_main_goes_without_a_second_upload() {
        const NOTE_OK: &str = r#"{"ok":true,"result":{"message_id":2,"video_note":{"file_id":"FILE1"}}}"#;
        let dir = test_dir("test_to_main");
        let file = test_file(&dir);
        let ffprobe = fake_ffprobe(&dir, 400);
        let server = api::fake::FakeTelegram::start(&[NOTE_OK, NOTE_OK]);

        let to_test = args(&file, &dir, &[(CHANNEL_FIELD, "test")]);
        assert_eq!(sent(&to_test, &server, &ffprobe).unwrap(), "Sent to Test");
        let to_main = args(&file, &dir, &[(CHANNEL_FIELD, "main")]);
        assert_eq!(sent(&to_main, &server, &ffprobe).unwrap(), "Sent to Main without uploading it again");

        let requests = server.requests();
        assert!(requests[0].text().contains("@test") && requests[0].text().contains("video_note"), "{}", requests[0].text());
        assert!(requests[0].header("content-type").unwrap().starts_with("multipart/"));
        assert_eq!(requests[1].text(), "chat_id=%40main&video_note=FILE1&length=400&duration=30");

        // Both are in the journal now.
        let journal = Journal::load(&dir);
        let key = journal::file_key(&file).unwrap();
        assert!(journal.warning(&key, "test", "Test", journal::now()).is_some());
        assert!(journal.warning(&key, "main", "Main", journal::now()).is_some());
        assert_eq!(journal.file_id(&key), Some("FILE1"));
    }

    #[cfg(windows)]
    #[test]
    fn nothing_goes_without_a_channel_or_with_a_problem() {
        let dir = test_dir("refusals");
        let file = test_file(&dir);
        let ffprobe = fake_ffprobe(&dir, 400);
        let server = api::fake::FakeTelegram::start(&[]);

        let error = sent(&args(&file, &dir, &[]), &server, &ffprobe).unwrap_err();
        assert!(format!("{error:#}").contains("No channel"), "{error:#}");
        let error = sent(&args(&file, &dir, &[(CHANNEL_FIELD, "other")]), &server, &ffprobe).unwrap_err();
        assert!(format!("{error:#}").contains("no channel"), "{error:#}");

        // The file is checked again at the last moment, whatever the form said.
        let error = sent(&args(&file, &dir, &[(CHANNEL_FIELD, "test")]), &server, &fake_ffprobe(&dir, 300)).unwrap_err();
        assert!(format!("{error:#}").contains("square"), "{error:#}");

        // A bad link stops it before the first message.
        let bad_link = args(&file, &dir, &[(CHANNEL_FIELD, "test"), (TEXT_FIELD, "t"), (LINK_FIELD, "no link")]);
        assert!(sent(&bad_link, &server, &ffprobe).is_err());

        assert!(server.methods().is_empty(), "{:?}", server.methods());
        assert_eq!(Journal::load(&dir), Journal::default());
    }

    #[cfg(windows)]
    #[test]
    fn a_journal_that_cant_be_written_doesnt_undo_the_sending() {
        let dir = test_dir("journal_fails");
        let file = test_file(&dir);
        let ffprobe = fake_ffprobe(&dir, 400);
        let server = api::fake::FakeTelegram::start(&[r#"{"ok":true,"result":{"message_id":2,"video_note":{"file_id":"F"}}}"#]);
        // A file where its folder should be.
        let not_a_folder = dir.join("not_a_folder");
        std::fs::write(&not_a_folder, "").unwrap();
        let text = sent(&args(&file, &not_a_folder, &[(CHANNEL_FIELD, "test")]), &server, &ffprobe).unwrap();
        assert!(text.starts_with("Sent to Test") && text.contains("journal"), "{text}");
    }

    #[test]
    fn the_end_says_what_really_happened() {
        let delivered = |round, uploaded| api::Delivered { file_id: None, round, uploaded };
        assert_eq!(outcome_text("Test", &delivered(true, true)), "Sent to Test");
        assert_eq!(outcome_text("Main", &delivered(true, false)), "Sent to Main without uploading it again");
        assert!(outcome_text("Test", &delivered(false, true)).contains("ordinary video"));
    }
}
