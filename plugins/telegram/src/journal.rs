//! What has been sent where. It warns before a file goes to the same channel twice, and remembers
//! Telegram's id for a file, so that it can go to another channel without being uploaded again.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const JOURNAL_FILE: &str = "sent.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Sending {
    channel: String,
    /// Seconds since 1970.
    at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Entry {
    name: String,
    /// Valid only for the bot that got it; an answer from Telegram says if it isn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_id: Option<String>,
    #[serde(default)]
    sent: Vec<Sending>,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    #[serde(default)]
    files: BTreeMap<String, Entry>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

/// What tells this file from others: a new export of the same name is another video.
pub fn file_key(file: &Path) -> Result<String> {
    let metadata = fs::metadata(file).with_context(|| format!("can't read {}", file.display()))?;
    let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |elapsed| elapsed.as_millis());
    Ok(format!("{}-{modified}", metadata.len()))
}

fn ago(seconds: u64) -> String {
    let (amount, unit) = match seconds {
        0..=59 => return "just now".to_owned(),
        60..=3599 => (seconds / 60, "minute"),
        3600..=86399 => (seconds / 3600, "hour"),
        _ => (seconds / 86400, "day"),
    };
    format!("{amount} {unit}{} ago", if amount == 1 { "" } else { "s" })
}

impl Journal {
    /// Reads the journal. A damaged one must not get in the way of sending: it is put aside.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(JOURNAL_FILE);
        let Ok(bytes) = fs::read(&path) else { return Self::default() };
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            let _ = fs::rename(&path, path.with_extension("json.bad"));
            Self::default()
        })
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        // In one step: a journal cut in the middle would forget everything.
        let temp = dir.join(format!("{JOURNAL_FILE}.tmp"));
        fs::write(&temp, serde_json::to_vec_pretty(self)?).with_context(|| format!("can't write {}", temp.display()))?;
        fs::rename(&temp, dir.join(JOURNAL_FILE)).with_context(|| format!("can't write the journal in {}", dir.display()))
    }

    pub fn file_id(&self, key: &str) -> Option<&str> {
        self.files.get(key)?.file_id.as_deref()
    }

    /// Says so if this file already went to this channel.
    pub fn warning(&self, key: &str, channel: &str, label: &str, now: u64) -> Option<String> {
        let last = self.files.get(key)?.sent.iter().filter(|sending| sending.channel == channel).map(|sending| sending.at).max()?;
        Some(format!("This file already went to {label} {}.", ago(now.saturating_sub(last))))
    }

    /// Notes a sending. Telegram's id replaces the one known before, or stays if there is no new one.
    pub fn record(&mut self, key: &str, name: &str, channel: &str, file_id: Option<String>, at: u64) {
        let entry = self.files.entry(key.to_owned()).or_default();
        entry.name = name.to_owned();
        entry.file_id = file_id.or(entry.file_id.take());
        entry.sent.push(Sending { channel: channel.to_owned(), at });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_telegram_journal_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn remembers_across_runs() {
        let dir = temp_dir("runs");
        assert_eq!(Journal::load(&dir), Journal::default());

        let mut journal = Journal::default();
        journal.record("k", "a.mp4", "test", Some("id1".into()), 1000);
        journal.save(&dir).unwrap();

        let loaded = Journal::load(&dir);
        assert_eq!(loaded, journal);
        assert_eq!(loaded.file_id("k"), Some("id1"));
        assert_eq!(loaded.file_id("other"), None);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "no temporary file stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn warns_only_about_the_same_file_in_the_same_channel() {
        let mut journal = Journal::default();
        journal.record("k", "a.mp4", "test", None, 1000);
        assert_eq!(journal.warning("k", "test", "Test", 1030).unwrap(), "This file already went to Test just now.");
        assert_eq!(journal.warning("k", "main", "Main", 1030), None);
        assert_eq!(journal.warning("other", "test", "Test", 1030), None);

        // The last time counts.
        journal.record("k", "a.mp4", "test", None, 1000 + 7200);
        assert_eq!(journal.warning("k", "test", "Test", 1000 + 7200 + 120).unwrap(), "This file already went to Test 2 minutes ago.");
    }

    #[test]
    fn says_how_long_ago() {
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(3599), "59 minutes ago");
        assert_eq!(ago(3600), "1 hour ago");
        assert_eq!(ago(86400 * 3), "3 days ago");
    }

    #[test]
    fn a_new_id_replaces_the_old_and_no_id_keeps_it() {
        let mut journal = Journal::default();
        journal.record("k", "a.mp4", "test", Some("old".into()), 1);
        journal.record("k", "a.mp4", "main", None, 2);
        assert_eq!(journal.file_id("k"), Some("old"));
        journal.record("k", "a.mp4", "main", Some("new".into()), 3);
        assert_eq!(journal.file_id("k"), Some("new"));
    }

    #[test]
    fn a_damaged_journal_is_put_aside() {
        let dir = temp_dir("damaged");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(JOURNAL_FILE), "{ not json").unwrap();
        assert_eq!(Journal::load(&dir), Journal::default());
        assert!(!dir.join(JOURNAL_FILE).exists());
        assert_eq!(fs::read_to_string(dir.join("sent.json.bad")).unwrap(), "{ not json");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_export_under_the_same_name_is_another_file() {
        let dir = temp_dir("key");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mp4");
        fs::write(&file, "one").unwrap();
        let first = file_key(&file).unwrap();
        assert_eq!(file_key(&file).unwrap(), first);
        fs::write(&file, "another").unwrap();
        assert_ne!(file_key(&file).unwrap(), first);
        assert!(file_key(&dir.join("no_such_file")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
