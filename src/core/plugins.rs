//! Plugins: separate programs that do what the editor itself has no business knowing about. The
//! editor knows only this protocol, and works the same without any plugin.
//!
//! A plugin is a program named `dicing_splicing-<id>` next to the editor, or in
//! `%APPDATA%\DicingSplicing\plugins`. It can be written in anything.
//!
//! `dicing_splicing-<id> --describe` prints what it is, as one JSON object:
//!
//! ```json
//! { "protocol": 1, "kind": "source", "name": "Button text", "prompt": "What to ask for",
//!   "hint": "A line of explanation",
//!   "options": [ { "key": "k", "label": "Label", "kind": "file", "hint": "Explanation" } ] }
//! ```
//!
//! The editor keeps the options among its settings, and passes them on as `--option key=value`.
//! Whatever a plugin is doing, it prints one JSON object per line, and anything else it prints is
//! ignored. `fraction` may be left out:
//!
//! ```json
//! { "event": "progress", "fraction": 0.5, "text": "Downloading" }
//! { "event": "error", "message": "What went wrong" }
//! ```
//!
//! When the standard input closes, the plugin should stop what it does and exit. The environment
//! variables `DICING_SPLICING_FFMPEG` and `DICING_SPLICING_FFPROBE` say which programs the editor uses.
//!
//! # The `source` kind brings in a video
//!
//! `dicing_splicing-<id> fetch --input <what the user typed> --out <folder>` does the work in `--out`,
//! which is the plugin's own folder in the workspace, and ends with:
//!
//! ```json
//! { "event": "done", "path": "C:\\video.mp4", "name": "Project name", "meta": { "title": "Song" } }
//! ```
//!
//! `name` may be left out. `meta` is kept with the project and not looked into.
//!
//! # The `publish` kind sends a finished file somewhere
//!
//! Its description adds `"extensions": ["mp4"]`, the files it takes; without it, any file. Both steps
//! are given `--file <path>` and `--data <folder>`, the plugin's own folder for its notes.
//!
//! `dicing_splicing-<id> prepare --name <project> [--meta key=value]...` says what to ask. `meta` is what
//! a source plugin said about the video. Nothing goes out yet:
//!
//! ```json
//! { "event": "form", "summary": "A line about the file",
//!   "fields": [
//!     { "key": "text", "label": "Text", "kind": "text", "value": "Starts as this", "hint": "Explanation" },
//!     { "key": "to", "label": "To", "kind": "choice", "value": "a", "choices": [
//!       { "value": "a", "label": "Place A", "action": "Send to A", "warnings": ["Only if A is chosen"] } ] } ],
//!   "problems": ["What stops it: the action can't be taken"],
//!   "warnings": ["What is worth knowing"],
//!   "action": "What the button says, unless a chosen value says otherwise" }
//! ```
//!
//! `dicing_splicing-<id> send [--field key=value]...` does it, with the values the user left in the form.
//! The plugin checks everything again: the form is only what things were like when it was made. It
//! ends with this, even after being asked to stop, because what has gone out has gone out:
//!
//! ```json
//! { "event": "done", "message": "What happened" }
//! ```
//!
//! This step is never forced to end: stopped halfway, it could leave half a post behind with nobody
//! knowing. So it has to end by itself, with time limits of its own. If it stops because it was asked
//! and leaves nothing behind, it says nothing; an `error` then means something is left to be seen to.

use crate::core::tools;
use crate::media::Notify;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const PROTOCOL: u32 = 1;
const NAME_PREFIX: &str = "dicing_splicing-";
const SOURCE_KIND: &str = "source";
const PUBLISH_KIND: &str = "publish";
const DEFAULT_ACTION: &str = "Send";
/// A plugin that doesn't say what it is in this time is not one.
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(3);
/// A plugin that doesn't stop in this time after being asked to is stopped.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
const WAIT_STEP: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionKind {
    #[default]
    Text,
    File,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PluginOption {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub kind: OptionKind,
    #[serde(default)]
    pub hint: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PluginKind {
    /// Brings in a video.
    Source,
    /// Sends a finished file somewhere.
    Publish,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plugin {
    pub id: String,
    pub program: PathBuf,
    pub kind: PluginKind,
    pub name: String,
    pub prompt: String,
    pub hint: String,
    pub options: Vec<PluginOption>,
    /// The files a publishing plugin takes. None named means any.
    pub extensions: Vec<String>,
}

impl Plugin {
    /// Whether this plugin can send this file somewhere.
    pub fn can_publish(&self, file: &Path) -> bool {
        let extension = file.extension().and_then(|e| e.to_str()).unwrap_or_default();
        self.kind == PluginKind::Publish && (self.extensions.is_empty() || self.extensions.iter().any(|known| known.eq_ignore_ascii_case(extension)))
    }
}

#[derive(Deserialize)]
struct Description {
    protocol: u32,
    kind: String,
    name: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    hint: String,
    #[serde(default)]
    options: Vec<PluginOption>,
    #[serde(default)]
    extensions: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    #[default]
    Text,
    Choice,
}

/// One of the values a field of the `choice` kind can have.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
    /// What the button says while this is chosen.
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub kind: FieldKind,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
}

/// What a publishing plugin wants to ask before it sends anything.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Form {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub fields: Vec<Field>,
    /// Any of these means it can't be sent.
    #[serde(default)]
    pub problems: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub action: String,
}

impl Form {
    /// The choices now chosen.
    fn chosen<'a>(&'a self, values: &'a BTreeMap<String, String>) -> impl Iterator<Item = &'a Choice> {
        self.fields.iter().flat_map(move |field| {
            let value = values.get(&field.key);
            field.choices.iter().filter(move |choice| Some(&choice.value) == value)
        })
    }

    /// What the button says with these values.
    pub fn action_text(&self, values: &BTreeMap<String, String>) -> String {
        let of_choice = self.chosen(values).find(|choice| !choice.action.is_empty()).map(|choice| choice.action.as_str());
        let text = of_choice.or(Some(self.action.as_str()).filter(|action| !action.is_empty())).unwrap_or(DEFAULT_ACTION);
        text.to_owned()
    }

    /// The warnings that apply with these values.
    pub fn warnings_for<'a>(&'a self, values: &'a BTreeMap<String, String>) -> Vec<&'a str> {
        let of_choices = self.chosen(values).flat_map(|choice| &choice.warnings);
        self.warnings.iter().chain(of_choices).map(String::as_str).collect()
    }

    /// The values the form starts with.
    pub fn initial_values(&self) -> BTreeMap<String, String> {
        self.fields.iter().map(|field| (field.key.clone(), field.value.clone())).collect()
    }

    /// The values after the form was made again: what the user left stays, as long as it still means
    /// something. A choice that is no longer there goes back to the start.
    pub fn keep_values(&self, previous: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let kept = |field: &Field| {
            let old = previous.get(&field.key)?;
            (field.kind != FieldKind::Choice || field.choices.iter().any(|choice| &choice.value == old)).then(|| old.clone())
        };
        self.fields.iter().map(|field| (field.key.clone(), kept(field).unwrap_or_else(|| field.value.clone()))).collect()
    }
}

/// A file to send, and what the plugin should know about it.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishTarget {
    pub file: PathBuf,
    /// The plugin's folder for its notes.
    pub data: PathBuf,
    pub name: String,
    pub meta: BTreeMap<String, String>,
}

/// Next to the editor, and among the user's own files.
pub fn default_folders() -> Vec<PathBuf> {
    let next_to_editor = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_owned));
    let in_user_files = crate::core::settings::app_folder().join("plugins");
    next_to_editor.into_iter().chain([in_user_files]).collect()
}

/// The id of a plugin, if this file is one.
fn plugin_id(path: &Path) -> Option<String> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    let runnable = if cfg!(windows) { matches!(extension.as_str(), "exe" | "cmd" | "bat") } else { extension.is_empty() };
    if !runnable {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    stem.strip_prefix(NAME_PREFIX).filter(|id| !id.is_empty()).map(str::to_owned)
}

fn parse_description(id: &str, program: &Path, json: &str) -> Result<Option<Plugin>, String> {
    let description: Description = serde_json::from_str(json).map_err(|error| format!("it says something unexpected: {error}"))?;
    if description.protocol != PROTOCOL {
        return Err(format!("it speaks protocol {}, this editor speaks {PROTOCOL}", description.protocol));
    }
    let kind = match description.kind.as_str() {
        SOURCE_KIND => PluginKind::Source,
        PUBLISH_KIND => PluginKind::Publish,
        _ => return Ok(None),
    };
    Ok(Some(Plugin {
        id: id.to_owned(),
        program: program.to_owned(),
        kind,
        name: description.name,
        prompt: description.prompt,
        hint: description.hint,
        options: description.options,
        extensions: description.extensions,
    }))
}

fn describe(id: &str, program: &Path) -> Result<Option<Plugin>, String> {
    let mut child = tools::command(&program.to_string_lossy())
        .arg("--describe")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("it doesn't start: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < DESCRIBE_TIMEOUT => thread::sleep(WAIT_STEP),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("it doesn't answer".to_owned());
            }
        }
    }
    let mut json = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut json);
    }
    parse_description(id, program, &json)
}

/// The plugins in these folders, and what is wrong with those that can't be used. A plugin of the
/// same id in a later folder doesn't count.
pub fn discover(folders: &[PathBuf]) -> (Vec<Plugin>, Vec<String>) {
    let (mut plugins, mut problems) = (Vec::<Plugin>::new(), Vec::new());
    for folder in folders {
        let Ok(entries) = fs::read_dir(folder) else { continue };
        let mut files: Vec<PathBuf> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
        files.sort();
        for file in files {
            let Some(id) = plugin_id(&file) else { continue };
            if plugins.iter().any(|plugin| plugin.id == id) {
                continue;
            }
            match describe(&id, &file) {
                Ok(Some(plugin)) => plugins.push(plugin),
                Ok(None) => {}
                Err(problem) => problems.push(format!("The plugin {} can't be used: {problem}", file.display())),
            }
        }
    }
    (plugins, problems)
}

/// What a plugin brought in.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub path: PathBuf,
    pub name: Option<String>,
    pub meta: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "lowercase")]
enum Event {
    Progress {
        #[serde(default)]
        fraction: Option<f32>,
        #[serde(default)]
        text: String,
    },
    Done {
        #[serde(default)]
        path: Option<PathBuf>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        meta: BTreeMap<String, String>,
        #[serde(default)]
        message: String,
    },
    Form(Form),
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Running { fraction: Option<f32>, text: String },
    /// A source plugin brought this in.
    Done(Fetched),
    /// A publishing plugin wants to ask this.
    Prepared(Form),
    /// A publishing plugin sent the file, and says so.
    Sent(String),
    Failed(String),
    Cancelled,
}

/// What a plugin is run for, which decides what counts as its result.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    Fetch,
    Prepare,
    Send,
}

impl Step {
    fn result_of(self, event: Event, plugin: &str) -> Option<RunStatus> {
        match (self, event) {
            (_, Event::Error { message }) => Some(RunStatus::Failed(message)),
            (Self::Fetch, Event::Done { path: Some(path), name, meta, .. }) if path.is_file() => Some(RunStatus::Done(Fetched { path, name, meta })),
            (Self::Fetch, Event::Done { path: Some(path), .. }) => Some(RunStatus::Failed(format!("{plugin} says it made {}, but there is no such file", path.display()))),
            (Self::Prepare, Event::Form(form)) => Some(RunStatus::Prepared(form)),
            (Self::Send, Event::Done { message, .. }) => Some(RunStatus::Sent(message)),
            _ => None,
        }
    }
}

struct Shared {
    status: Mutex<RunStatus>,
    cancelled: AtomicBool,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
}

impl Shared {
    fn set(&self, status: RunStatus) {
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }

    /// Waits for the program to end, without keeping others from stopping it.
    fn wait_for_exit(&self) -> bool {
        loop {
            let finished = match self.child.lock() {
                Ok(mut child) => child.as_mut().map(|child| child.try_wait()),
                Err(_) => return false,
            };
            match finished {
                Some(Ok(Some(status))) => return status.success(),
                Some(Ok(None)) => thread::sleep(WAIT_STEP),
                _ => return false,
            }
        }
    }
}

/// A plugin doing its work in the background. Dropping this asks it to stop.
pub struct PluginRun {
    shared: Arc<Shared>,
    step: Step,
}

/// Adds `flag key=value` for each of these.
fn add_pairs(command: &mut Command, flag: &str, pairs: &BTreeMap<String, String>) {
    for (key, value) in pairs {
        command.arg(flag).arg(format!("{key}={value}"));
    }
}

impl PluginRun {
    /// Has a source plugin bring in what the user asked for.
    pub fn fetch(plugin: &Plugin, input: &str, out: &Path, options: &BTreeMap<String, String>, notify: Notify) -> Self {
        let mut command = tools::command(&plugin.program.to_string_lossy());
        command.args(["fetch", "--input", input, "--out"]).arg(out);
        add_pairs(&mut command, "--option", options);
        Self::launch(plugin, command, Step::Fetch, notify)
    }

    fn publish_command(plugin: &Plugin, step: &str, target: &PublishTarget, options: &BTreeMap<String, String>) -> Command {
        let mut command = tools::command(&plugin.program.to_string_lossy());
        command.arg(step).arg("--file").arg(&target.file).arg("--data").arg(&target.data);
        add_pairs(&mut command, "--option", options);
        command
    }

    /// Asks a publishing plugin what it wants to know. Nothing goes out.
    pub fn prepare(plugin: &Plugin, target: &PublishTarget, options: &BTreeMap<String, String>, notify: Notify) -> Self {
        let mut command = Self::publish_command(plugin, "prepare", target, options);
        command.arg("--name").arg(&target.name);
        add_pairs(&mut command, "--meta", &target.meta);
        Self::launch(plugin, command, Step::Prepare, notify)
    }

    /// Has a publishing plugin send the file.
    pub fn send(plugin: &Plugin, target: &PublishTarget, fields: &BTreeMap<String, String>, options: &BTreeMap<String, String>, notify: Notify) -> Self {
        let mut command = Self::publish_command(plugin, "send", target, options);
        add_pairs(&mut command, "--field", fields);
        Self::launch(plugin, command, Step::Send, notify)
    }

    fn launch(plugin: &Plugin, mut command: Command, step: Step, notify: Notify) -> Self {
        let shared = Arc::new(Shared {
            status: Mutex::new(RunStatus::Running { fraction: None, text: String::new() }),
            cancelled: AtomicBool::new(false),
            stdin: Mutex::new(None),
            child: Mutex::new(None),
        });
        command.env("DICING_SPLICING_FFMPEG", tools::program_path(tools::FFMPEG));
        command.env("DICING_SPLICING_FFPROBE", tools::program_path(tools::FFPROBE));
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

        let name = plugin.name.clone();
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                shared.set(RunStatus::Failed(format!("{name} doesn't start: {error}")));
                return Self { shared, step };
            }
        };
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        if let Ok(mut stdin) = shared.stdin.lock() {
            *stdin = child.stdin.take();
        }
        if let Ok(mut slot) = shared.child.lock() {
            *slot = Some(child);
        }

        let worker = shared.clone();
        thread::spawn(move || {
            let errors = thread::spawn(move || {
                let mut text = String::new();
                if let Some(mut stderr) = stderr {
                    let _ = stderr.read_to_string(&mut text);
                }
                text
            });
            let mut outcome = None;
            if let Some(stdout) = stdout {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    match serde_json::from_str::<Event>(&line) {
                        Ok(Event::Progress { fraction, text }) => {
                            worker.set(RunStatus::Running { fraction: fraction.map(|f| f.clamp(0.0, 1.0)), text });
                            notify();
                        }
                        Ok(event) => outcome = step.result_of(event, &name).or(outcome),
                        Err(_) => {}
                    }
                }
            }
            let exited_well = worker.wait_for_exit();
            let errors = errors.join().unwrap_or_default();
            let status = match outcome {
                // Stopping came too late: saying "cancelled" now would be untrue.
                Some(sent @ RunStatus::Sent(_)) => sent,
                // Something went wrong on the way to stopping, and the user has to hear about it.
                Some(failed @ RunStatus::Failed(_)) if step == Step::Send => failed,
                _ if worker.cancelled.load(Ordering::SeqCst) => RunStatus::Cancelled,
                Some(status) => status,
                None if !errors.trim().is_empty() => RunStatus::Failed(errors.trim().to_owned()),
                None if exited_well => RunStatus::Failed(format!("{name} finished without giving a result")),
                None => RunStatus::Failed(format!("{name} stopped with an error")),
            };
            worker.set(status);
            notify();
        });
        Self { shared, step }
    }

    pub fn status(&self) -> RunStatus {
        self.shared.status.lock().map_or(RunStatus::Cancelled, |status| status.clone())
    }

    /// Asks the plugin to stop by closing its input, and stops it if it doesn't. Except when it is
    /// sending: see the protocol.
    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::SeqCst);
        let closed = self.shared.stdin.lock().ok().and_then(|mut stdin| stdin.take());
        if closed.is_none() {
            return;
        }
        drop(closed);
        if self.step == Step::Send {
            return;
        }
        let shared = self.shared.clone();
        thread::spawn(move || {
            thread::sleep(CANCEL_TIMEOUT);
            if let Ok(mut child) = shared.child.lock() {
                if let Some(child) = child.as_mut() {
                    tools::kill_tree(child);
                }
            }
        });
    }
}

impl Drop for PluginRun {
    fn drop(&mut self) {
        if matches!(self.status(), RunStatus::Running { .. }) {
            self.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_plugins_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plugins_are_known_by_their_names() {
        assert_eq!(plugin_id(Path::new("d/dicing_splicing-link.exe")).as_deref(), Some("link"));
        assert_eq!(plugin_id(Path::new("dicing_splicing-a-b.cmd")).as_deref(), Some("a-b"));
        assert_eq!(plugin_id(Path::new("dicing_splicing.exe")), None);
        assert_eq!(plugin_id(Path::new("dicing_splicing-.exe")), None);
        assert_eq!(plugin_id(Path::new("dicing_splicing-link.pdb")), None);
        assert_eq!(plugin_id(Path::new("dicing_splicing-link.d")), None);
        assert_eq!(plugin_id(Path::new("other.exe")), None);
    }

    #[test]
    fn reads_a_description() {
        let json = r#"{"protocol":1,"kind":"source","name":"N","prompt":"P","options":[{"key":"k","label":"L","kind":"file"},{"key":"t","label":"T"}]}"#;
        let plugin = parse_description("x", Path::new("x.exe"), json).unwrap().unwrap();
        assert_eq!((plugin.id.as_str(), plugin.name.as_str(), plugin.prompt.as_str()), ("x", "N", "P"));
        assert_eq!(plugin.kind, PluginKind::Source);
        assert_eq!(plugin.options[0].kind, OptionKind::File);
        assert_eq!(plugin.options[1].kind, OptionKind::Text);
    }

    fn publisher(extensions: &[&str]) -> Plugin {
        let description = serde_json::json!({ "protocol": 1, "kind": "publish", "name": "P", "extensions": extensions }).to_string();
        parse_description("p", Path::new("p.exe"), &description).unwrap().unwrap()
    }

    #[test]
    fn a_publishing_plugin_says_which_files_it_takes() {
        let plugin = publisher(&["mp4", "mov"]);
        assert_eq!(plugin.kind, PluginKind::Publish);
        assert!(plugin.can_publish(Path::new("a/clip.mp4")));
        assert!(plugin.can_publish(Path::new("a/CLIP.MOV")));
        assert!(!plugin.can_publish(Path::new("a/frame.png")));
        assert!(!plugin.can_publish(Path::new("a/no_extension")));
        // Saying nothing means anything.
        assert!(publisher(&[]).can_publish(Path::new("a/frame.png")));

        let source = parse_description("s", Path::new("s"), r#"{"protocol":1,"kind":"source","name":"S"}"#).unwrap().unwrap();
        assert!(!source.can_publish(Path::new("a/clip.mp4")), "it brings things in");
    }

    fn form() -> Form {
        serde_json::from_str(
            r#"{"summary":"s","action":"Go","warnings":["always"],"fields":[
                {"key":"text","label":"Text","value":"start","hint":"h"},
                {"key":"to","label":"To","kind":"choice","value":"a","choices":[
                    {"value":"a","label":"A","action":"Send to A"},
                    {"value":"b","label":"B","action":"Send to B","warnings":["b was used"]},
                    {"value":"c","label":"C"}]}]}"#,
        )
        .unwrap()
    }

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
    }

    #[test]
    fn the_button_and_the_warnings_follow_the_choice() {
        let form = form();
        assert_eq!(form.action_text(&values(&[("to", "a")])), "Send to A");
        assert_eq!(form.action_text(&values(&[("to", "b")])), "Send to B");
        // A choice that doesn't name it leaves the form's own text, or a plain one.
        assert_eq!(form.action_text(&values(&[("to", "c")])), "Go");
        assert_eq!(Form::default().action_text(&values(&[])), DEFAULT_ACTION);

        assert_eq!(form.warnings_for(&values(&[("to", "a")])), ["always"]);
        assert_eq!(form.warnings_for(&values(&[("to", "b")])), ["always", "b was used"]);
    }

    #[test]
    fn a_form_made_again_keeps_what_the_user_left() {
        let form = form();
        assert_eq!(form.initial_values(), values(&[("text", "start"), ("to", "a")]));
        let typed = values(&[("text", "typed"), ("to", "b"), ("gone", "x")]);
        assert_eq!(form.keep_values(&typed), values(&[("text", "typed"), ("to", "b")]));
        // A choice that is no longer offered can't stay chosen.
        assert_eq!(form.keep_values(&values(&[("to", "removed")])), values(&[("text", "start"), ("to", "a")]));
    }

    #[test]
    fn a_kind_it_doesnt_know_is_left_alone_and_another_protocol_is_reported() {
        let other_kind = r#"{"protocol":1,"kind":"something_else","name":"N"}"#;
        assert_eq!(parse_description("x", Path::new("x"), other_kind).unwrap(), None);
        let other_protocol = r#"{"protocol":2,"kind":"source","name":"N"}"#;
        assert!(parse_description("x", Path::new("x"), other_protocol).unwrap_err().contains("protocol 2"));
        assert!(parse_description("x", Path::new("x"), "not json").is_err());
    }

    #[test]
    fn reads_events() {
        let read = |line: &str| serde_json::from_str::<Event>(line).unwrap();
        assert_eq!(read(r#"{"event":"progress","fraction":0.5,"text":"T"}"#), Event::Progress { fraction: Some(0.5), text: "T".into() });
        assert_eq!(read(r#"{"event":"progress","fraction":null}"#), Event::Progress { fraction: None, text: String::new() });
        assert_eq!(read(r#"{"event":"done","path":"a"}"#), Event::Done { path: Some("a".into()), name: None, meta: BTreeMap::new(), message: String::new() });
        assert_eq!(read(r#"{"event":"done","message":"Sent"}"#), Event::Done { path: None, name: None, meta: BTreeMap::new(), message: "Sent".into() });
        assert_eq!(read(r#"{"event":"form","summary":"S"}"#), Event::Form(Form { summary: "S".into(), ..Form::default() }));
        assert_eq!(read(r#"{"event":"error","message":"M"}"#), Event::Error { message: "M".into() });
        assert!(serde_json::from_str::<Event>("[download] 5%").is_err());
    }

    /// A plugin as a batch file: what it does depends on the input it is given.
    #[cfg(windows)]
    fn fake_plugin(dir: &Path) -> PathBuf {
        let video = dir.join("video.mp4");
        fs::write(&video, "video").unwrap();
        let path = dir.join("dicing_splicing-fake.cmd");
        let video = video.display().to_string().replace('\\', "\\\\");
        let script = format!(
            "@echo off\r\n\
             if \"%1\"==\"--describe\" goto describe\r\n\
             if \"%3\"==\"good\" goto good\r\n\
             if \"%3\"==\"bad\" goto bad\r\n\
             if \"%3\"==\"silent\" goto silent\r\n\
             if \"%3\"==\"nofile\" goto nofile\r\n\
             if \"%3\"==\"wait\" goto wait\r\n\
             exit /b 3\r\n\
             :describe\r\n\
             echo {{\"protocol\":1,\"kind\":\"source\",\"name\":\"Fake\",\"prompt\":\"P\"}}\r\n\
             exit /b 0\r\n\
             :good\r\n\
             echo not an event\r\n\
             echo {{\"event\":\"progress\",\"fraction\":0.5,\"text\":\"Half\"}}\r\n\
             echo {{\"event\":\"done\",\"path\":\"{video}\",\"name\":\"N\",\"meta\":{{\"a\":\"b\"}}}}\r\n\
             exit /b 0\r\n\
             :bad\r\n\
             echo {{\"event\":\"error\",\"message\":\"It went wrong\"}}\r\n\
             exit /b 1\r\n\
             :silent\r\n\
             exit /b 0\r\n\
             :nofile\r\n\
             echo {{\"event\":\"done\",\"path\":\"no_such_file.mp4\"}}\r\n\
             exit /b 0\r\n\
             :wait\r\n\
             echo {{\"event\":\"progress\",\"text\":\"Waiting\"}}\r\n\
             more > nul\r\n\
             exit /b 0\r\n"
        );
        fs::write(&path, script).unwrap();
        path
    }

    #[cfg(windows)]
    fn wait_for_end(run: &PluginRun) -> RunStatus {
        let started = Instant::now();
        loop {
            let status = run.status();
            if !matches!(status, RunStatus::Running { .. }) {
                return status;
            }
            assert!(started.elapsed() < Duration::from_secs(30), "the plugin does not end");
            thread::sleep(WAIT_STEP);
        }
    }

    #[cfg(windows)]
    #[test]
    fn finds_and_runs_a_plugin() {
        let dir = temp_dir("run");
        fake_plugin(&dir);
        fs::write(dir.join("dicing_splicing-broken.cmd"), "@echo not json\r\n").unwrap();
        fs::write(dir.join("unrelated.cmd"), "@exit /b 1\r\n").unwrap();

        let (plugins, problems) = discover(&[dir.clone(), dir.join("no_such_folder")]);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].id, "fake");
        assert_eq!(plugins[0].name, "Fake");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("dicing_splicing-broken"), "{problems:?}");
        let plugin = &plugins[0];

        let notify: Notify = Arc::new(|| {});
        let options = BTreeMap::from([("k".to_owned(), "v".to_owned())]);
        let run = |input: &str| PluginRun::fetch(plugin, input, &dir, &options, notify.clone());

        match wait_for_end(&run("good")) {
            RunStatus::Done(fetched) => {
                assert_eq!(fetched.path, dir.join("video.mp4"));
                assert_eq!(fetched.name.as_deref(), Some("N"));
                assert_eq!(fetched.meta["a"], "b");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(wait_for_end(&run("bad")), RunStatus::Failed("It went wrong".into()));
        assert!(matches!(wait_for_end(&run("silent")), RunStatus::Failed(message) if message.contains("without giving a result")));
        assert!(matches!(wait_for_end(&run("nofile")), RunStatus::Failed(message) if message.contains("no such file")));
        assert!(matches!(wait_for_end(&run("other")), RunStatus::Failed(message) if message.contains("stopped with an error")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn closing_the_input_stops_a_plugin() {
        let dir = temp_dir("cancel");
        fake_plugin(&dir);
        let (plugins, _) = discover(std::slice::from_ref(&dir));
        let notify: Notify = Arc::new(|| {});
        let run = PluginRun::fetch(&plugins[0], "wait", &dir, &BTreeMap::new(), notify);

        let started = Instant::now();
        while !matches!(run.status(), RunStatus::Running { fraction: None, ref text } if text == "Waiting") {
            assert!(started.elapsed() < Duration::from_secs(10), "the plugin does not start");
            thread::sleep(WAIT_STEP);
        }
        run.cancel();
        assert_eq!(wait_for_end(&run), RunStatus::Cancelled);
        assert!(started.elapsed() < CANCEL_TIMEOUT, "it should have ended by itself");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A publishing plugin as a batch file: what it sends depends on the name of the file.
    #[cfg(windows)]
    fn fake_publisher(dir: &Path) -> Plugin {
        let path = dir.join("dicing_splicing-pub.cmd");
        let script = format!(
            "@echo off\r\n\
             if \"%1\"==\"--describe\" goto describe\r\n\
             echo %* > \"{args}\"\r\n\
             if \"%1\"==\"prepare\" goto prepare\r\n\
             if \"%3\"==\"good\" goto good\r\n\
             if \"%3\"==\"bad\" goto bad\r\n\
             if \"%3\"==\"late\" goto late\r\n\
             if \"%3\"==\"wait\" goto wait\r\n\
             if \"%3\"==\"messy\" goto messy\r\n\
             if \"%3\"==\"slow\" goto slow\r\n\
             exit /b 3\r\n\
             :describe\r\n\
             echo {{\"protocol\":1,\"kind\":\"publish\",\"name\":\"Pub\",\"extensions\":[\"mp4\"]}}\r\n\
             exit /b 0\r\n\
             :prepare\r\n\
             echo {{\"event\":\"form\",\"summary\":\"S\",\"problems\":[\"P\"]}}\r\n\
             exit /b 0\r\n\
             :good\r\n\
             echo {{\"event\":\"progress\",\"fraction\":0.5,\"text\":\"Half\"}}\r\n\
             echo {{\"event\":\"done\",\"message\":\"Went out\"}}\r\n\
             exit /b 0\r\n\
             :bad\r\n\
             echo {{\"event\":\"error\",\"message\":\"It went wrong\"}}\r\n\
             exit /b 1\r\n\
             :late\r\n\
             echo {{\"event\":\"progress\",\"text\":\"Waiting\"}}\r\n\
             more > nul\r\n\
             echo {{\"event\":\"done\",\"message\":\"Went out anyway\"}}\r\n\
             exit /b 0\r\n\
             :wait\r\n\
             echo {{\"event\":\"progress\",\"text\":\"Waiting\"}}\r\n\
             more > nul\r\n\
             exit /b 1\r\n\
             :messy\r\n\
             echo {{\"event\":\"progress\",\"text\":\"Waiting\"}}\r\n\
             more > nul\r\n\
             echo {{\"event\":\"error\",\"message\":\"Something stayed behind\"}}\r\n\
             exit /b 1\r\n\
             :slow\r\n\
             echo {{\"event\":\"progress\",\"text\":\"Waiting\"}}\r\n\
             more > nul\r\n\
             ping -n {slow_pings} 127.0.0.1 > nul\r\n\
             echo {{\"event\":\"done\",\"message\":\"Took its time\"}}\r\n\
             exit /b 0\r\n",
            args = dir.join("args.txt").display(),
            // One a second, the first at once: longer than a plugin that is not sending is given.
            slow_pings = CANCEL_TIMEOUT.as_secs() + 2,
        );
        fs::write(&path, script).unwrap();
        let (mut plugins, problems) = discover(&[dir.to_owned()]);
        assert_eq!(problems, Vec::<String>::new());
        plugins.remove(0)
    }

    #[cfg(windows)]
    fn target(dir: &Path, file: &str) -> PublishTarget {
        let meta = BTreeMap::from([("title".to_owned(), "Song".to_owned())]);
        PublishTarget { file: file.into(), data: dir.join("data"), name: "Project".into(), meta }
    }

    #[cfg(windows)]
    #[test]
    fn preparing_asks_the_plugin_what_to_ask() {
        let dir = temp_dir("prepare");
        let plugin = fake_publisher(&dir);
        assert_eq!(plugin.kind, PluginKind::Publish);
        let notify: Notify = Arc::new(|| {});
        let run = PluginRun::prepare(&plugin, &target(&dir, "clip.mp4"), &BTreeMap::new(), notify);
        match wait_for_end(&run) {
            RunStatus::Prepared(form) => {
                assert_eq!(form.summary, "S");
                assert_eq!(form.problems, ["P"]);
            }
            other => panic!("{other:?}"),
        }
        let args = fs::read_to_string(dir.join("args.txt")).unwrap();
        for part in ["prepare", "--file clip.mp4", "--data", "--name Project", "--meta", "title=Song"] {
            assert!(args.contains(part), "{part} is not in {args}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn sending_passes_on_the_fields_and_reports_what_happened() {
        let dir = temp_dir("send");
        let plugin = fake_publisher(&dir);
        let notify: Notify = Arc::new(|| {});
        let fields = values(&[("to", "a")]);
        let options = values(&[("k", "v")]);
        let run = |file: &str| PluginRun::send(&plugin, &target(&dir, file), &fields, &options, notify.clone());

        assert_eq!(wait_for_end(&run("good")), RunStatus::Sent("Went out".into()));
        let args = fs::read_to_string(dir.join("args.txt")).unwrap();
        for part in ["send", "--file good", "--field", "to=a", "--option", "k=v"] {
            assert!(args.contains(part), "{part} is not in {args}");
        }
        assert!(!args.contains("--meta") && !args.contains("--name"), "{args}");
        assert_eq!(wait_for_end(&run("bad")), RunStatus::Failed("It went wrong".into()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    fn run_and_cancel(plugin: &Plugin, dir: &Path, file: &str) -> RunStatus {
        let notify: Notify = Arc::new(|| {});
        let run = PluginRun::send(plugin, &target(dir, file), &BTreeMap::new(), &BTreeMap::new(), notify);
        let started = Instant::now();
        while !matches!(run.status(), RunStatus::Running { ref text, .. } if text == "Waiting") {
            assert!(started.elapsed() < Duration::from_secs(10), "the plugin does not start");
            thread::sleep(WAIT_STEP);
        }
        run.cancel();
        wait_for_end(&run)
    }

    #[cfg(windows)]
    #[test]
    fn what_went_out_is_not_called_cancelled() {
        let dir = temp_dir("late");
        let plugin = fake_publisher(&dir);
        assert_eq!(run_and_cancel(&plugin, &dir, "late"), RunStatus::Sent("Went out anyway".into()));
        assert_eq!(run_and_cancel(&plugin, &dir, "wait"), RunStatus::Cancelled);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn what_stopping_left_behind_is_told() {
        let dir = temp_dir("messy");
        let plugin = fake_publisher(&dir);
        assert_eq!(run_and_cancel(&plugin, &dir, "messy"), RunStatus::Failed("Something stayed behind".into()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_plugin_that_is_sending_is_never_forced_to_end() {
        let dir = temp_dir("slow");
        let plugin = fake_publisher(&dir);
        assert_eq!(run_and_cancel(&plugin, &dir, "slow"), RunStatus::Sent("Took its time".into()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plugin_that_doesnt_start_says_so() {
        let plugin = Plugin {
            id: "x".into(),
            program: "no_such_program_at_all".into(),
            kind: PluginKind::Source,
            name: "X".into(),
            prompt: String::new(),
            hint: String::new(),
            options: Vec::new(),
            extensions: Vec::new(),
        };
        let notify: Notify = Arc::new(|| {});
        let run = PluginRun::fetch(&plugin, "a", Path::new("."), &BTreeMap::new(), notify);
        assert!(matches!(run.status(), RunStatus::Failed(message) if message.contains("doesn't start")));
    }
}
