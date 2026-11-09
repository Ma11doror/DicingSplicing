//! The bot's token and channels. They are read from a file of their own, by this program only: the
//! token never goes on a command line, is never told to the editor and never shows in an error.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "credentials.json";
const DEFAULT_API: &str = "https://api.telegram.org";
const HIDDEN: &str = "<token>";
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
/// How many folders to look through, up from the program. A build is in `target/debug`, two folders
/// under the project.
const FOLDERS_TO_SEARCH: usize = 3;

/// A chat's id may be written as text (`@channel`) or as a number (`-100123`).
fn text_or_number<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) => text.trim().to_owned(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    })
}

/// The file, in the format of tgCircleGen's `config.json`. `api_url` is for a Bot API server of your
/// own, or a pretend one.
#[derive(Deserialize)]
struct Stored {
    #[serde(default, deserialize_with = "text_or_number")]
    bot_token: String,
    #[serde(default, deserialize_with = "text_or_number")]
    chat_id: String,
    #[serde(default, deserialize_with = "text_or_number")]
    chat_id_test: String,
    #[serde(default, deserialize_with = "text_or_number")]
    api_url: String,
}

/// Where a video can go.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    /// What the form and the journal call it.
    pub id: &'static str,
    pub label: &'static str,
    pub chat: String,
}

/// No `Debug` on purpose: nothing should be able to print this by accident.
pub struct Credentials {
    token: String,
    main: String,
    test: String,
    api_url: String,
}

/// A token is digits, a colon and letters. Anything else in the address would change where it goes.
fn looks_like_a_token(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-'))
}

/// Takes the token out of a text, also in the form it has in an address.
pub fn scrub(text: &str, token: &str) -> String {
    text.replace(token, HIDDEN).replace(&token.replace(':', "%3A"), HIDDEN)
}

impl Credentials {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        // Notepad likes to start a file with this.
        let bytes = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
        // Not the parser's own message: it may quote what is in the file.
        let stored: Stored = serde_json::from_slice(bytes).map_err(|error| anyhow!("it is not valid JSON (see line {})", error.line()))?;
        if stored.bot_token.is_empty() {
            bail!("it has no bot_token");
        }
        if !looks_like_a_token(&stored.bot_token) {
            bail!("its bot_token doesn't look like a bot's token");
        }
        if stored.chat_id.is_empty() && stored.chat_id_test.is_empty() {
            bail!("it has neither chat_id nor chat_id_test");
        }
        let api_url = if stored.api_url.is_empty() { DEFAULT_API.to_owned() } else { stored.api_url };
        Ok(Self { token: stored.bot_token, main: stored.chat_id, test: stored.chat_id_test, api_url: api_url.trim_end_matches('/').to_owned() })
    }

    /// Reads the file the user chose, or else the one found near the program.
    pub fn load(chosen: Option<&str>) -> Result<Self> {
        let exe = std::env::current_exe().context("can't tell where this program is")?;
        let path = locate(chosen, &exe)?;
        let bytes = fs::read(&path).with_context(|| format!("can't read {}", path.display()))?;
        Self::parse(&bytes).map_err(|error| anyhow!("{}: {error}", path.display()))
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    /// The channels there are, the test one first.
    pub fn channels(&self) -> Vec<Channel> {
        let all = [("test", "Test", &self.test), ("main", "Main", &self.main)];
        all.into_iter().filter(|(_, _, chat)| !chat.is_empty()).map(|(id, label, chat)| Channel { id, label, chat: chat.clone() }).collect()
    }

    pub fn channel(&self, id: &str) -> Result<Channel> {
        self.channels().into_iter().find(|channel| channel.id == id).with_context(|| format!("There is no channel \"{id}\" in {FILE_NAME}"))
    }

    pub fn scrub(&self, text: &str) -> String {
        scrub(text, &self.token)
    }
}

/// The folders to look in, up from the program.
fn search_folders(exe: &Path) -> Vec<&Path> {
    exe.ancestors().skip(1).take(FOLDERS_TO_SEARCH).collect()
}

fn locate(chosen: Option<&str>, exe: &Path) -> Result<PathBuf> {
    if let Some(chosen) = chosen {
        return Ok(PathBuf::from(chosen));
    }
    let folders = search_folders(exe);
    if let Some(found) = folders.iter().map(|folder| folder.join(FILE_NAME)).find(|path| path.is_file()) {
        return Ok(found);
    }
    let suggested = folders.last().map_or_else(|| ".".to_owned(), |folder| folder.display().to_string());
    bail!("There is no {FILE_NAME}. Put one in {suggested}, or choose the file in the Settings. It holds bot_token, chat_id and chat_id_test.")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "123456:fake-Token_value";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_telegram_credentials_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn error_of(json: &str) -> String {
        format!("{:#}", Credentials::parse(json.as_bytes()).err().expect("it should not be accepted"))
    }

    #[test]
    fn reads_the_format_of_the_old_script() {
        let json = format!(r#"{{"bot_token":"{TOKEN}","chat_id":"@main","chat_id_test":"@test"}}"#);
        let credentials = Credentials::parse(json.as_bytes()).unwrap();
        assert_eq!(credentials.token(), TOKEN);
        assert_eq!(credentials.api_url(), DEFAULT_API);
        let channels = credentials.channels();
        assert_eq!(channels[0], Channel { id: "test", label: "Test", chat: "@test".into() });
        assert_eq!(channels[1], Channel { id: "main", label: "Main", chat: "@main".into() });
    }

    #[test]
    fn takes_a_number_for_a_chat_and_a_mark_at_the_start_of_the_file() {
        let json = format!("\u{feff}{{\"bot_token\":\"{TOKEN}\",\"chat_id\":-100123}}");
        let credentials = Credentials::parse(json.as_bytes()).unwrap();
        assert_eq!(credentials.channels().len(), 1);
        assert_eq!(credentials.channel("main").unwrap().chat, "-100123");
        assert!(credentials.channel("test").is_err(), "no such channel in this file");
        assert!(credentials.channel("anything").is_err());
    }

    #[test]
    fn another_server_can_be_named() {
        let json = format!(r#"{{"bot_token":"{TOKEN}","chat_id":"1","api_url":"http://127.0.0.1:81/"}}"#);
        assert_eq!(Credentials::parse(json.as_bytes()).unwrap().api_url(), "http://127.0.0.1:81");
    }

    #[test]
    fn what_is_wrong_is_said_without_quoting_the_file() {
        assert!(error_of(r#"{"chat_id":"1"}"#).contains("no bot_token"));
        assert!(error_of(r#"{"bot_token":"123:abc"}"#).contains("neither chat_id"));
        // Something a parser would quote: the secret must not come out with the complaint.
        for broken in [format!(r#"{{"bot_token":"{TOKEN}""#), format!(r#"["{TOKEN}"]"#), format!(r#""{TOKEN}""#)] {
            assert!(!error_of(&broken).contains(TOKEN), "{broken}");
        }
        let in_address = r#"{"bot_token":"123/../other?x=1","chat_id":"1"}"#;
        let error = error_of(in_address);
        assert!(error.contains("doesn't look like"), "{error}");
        assert!(!error.contains("other"), "{error}");
    }

    #[test]
    fn the_token_is_taken_out_of_any_text() {
        assert_eq!(scrub(&format!("POST https://x/bot{TOKEN}/send failed"), TOKEN), "POST https://x/bot<token>/send failed");
        assert_eq!(scrub("POST https://x/bot123456%3Afake-Token_value/send", TOKEN), "POST https://x/bot<token>/send");
    }

    #[test]
    fn looks_near_the_program_and_no_further_than_the_project() {
        let root = temp_dir("locate");
        let exe = root.join("project").join("target").join("debug").join("plugin.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();

        let error = format!("{:#}", locate(None, &exe).unwrap_err());
        assert!(error.contains(&root.join("project").display().to_string()), "{error}");

        // Above the project doesn't count.
        fs::write(root.join(FILE_NAME), "{}").unwrap();
        assert!(locate(None, &exe).is_err());

        let in_project = root.join("project").join(FILE_NAME);
        fs::write(&in_project, "{}").unwrap();
        assert_eq!(locate(None, &exe).unwrap(), in_project);

        // The nearest one wins, and the one the user chose beats them all.
        let next_to_program = exe.parent().unwrap().join(FILE_NAME);
        fs::write(&next_to_program, "{}").unwrap();
        assert_eq!(locate(None, &exe).unwrap(), next_to_program);
        assert_eq!(locate(Some("elsewhere/my.json"), &exe).unwrap(), PathBuf::from("elsewhere/my.json"));
        let _ = fs::remove_dir_all(&root);
    }
}
