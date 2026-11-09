//! Reports of what went wrong. A released editor has no console: when it closes on an error, a report
//! in the logs folder is all there is to send to whoever looks into it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The editor's version, as the window's title and the reports name it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const PREFIX: &str = "crash-";

/// `%APPDATA%\DicingSplicing\logs`.
pub fn logs_folder() -> PathBuf {
    crate::core::settings::app_folder().join("logs")
}

/// From now on a panic in any thread also writes a report into `folder`, and then goes on as before.
pub fn install(folder: PathBuf) {
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic without a message".to_owned());
        let place = info.location().map_or_else(|| "an unknown place".to_owned(), |place| format!("{}:{}", place.file(), place.line()));
        let thread = std::thread::current().name().unwrap_or("unnamed").to_owned();
        let stack = std::backtrace::Backtrace::force_capture().to_string();
        let _ = write_report(&folder, &report(&message, &place, &thread, &stack, now()));
        before(info);
    }));
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// The text of a report.
fn report(message: &str, place: &str, thread: &str, stack: &str, at: u64) -> String {
    format!(
        "DicingSplicing {VERSION} on {} ({})\nWhen: {} UTC\nThread: {thread}\nWhat: {message}\nWhere: {place}\n\nStack:\n{stack}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        utc(at),
    )
}

/// Writes a report named by the moment, and returns where.
fn write_report(folder: &Path, text: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(folder)?;
    let name = format!("{PREFIX}{}-{}.txt", utc(now()).replace([':', ' '], "-"), std::process::id());
    let path = folder.join(name);
    fs::write(&path, text)?;
    Ok(path)
}

/// The newest report in `folder`. Their names sort by time.
pub fn newest_report(folder: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(folder).ok()?;
    let reports = entries.filter_map(Result::ok).map(|entry| entry.path());
    reports.filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(PREFIX))).max()
}

/// `2026-11-07 18:05:07` for seconds since 1970, in UTC.
fn utc(seconds: u64) -> String {
    let (days, rest) = ((seconds / 86_400) as i64, seconds % 86_400);
    // Days to a date in the proleptic Gregorian calendar (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// Shows `path` in the file manager: the folder, with the file picked if it is one.
pub fn show_in_folder(path: &Path) {
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new("explorer");
        if path.is_file() {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(path);
        }
        let _ = command.spawn();
    }
    #[cfg(not(windows))]
    {
        let folder = if path.is_file() { path.parent().unwrap_or(path) } else { path };
        let _ = std::process::Command::new("xdg-open").arg(folder).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_come_out_right() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(utc(1_794_075_907), "2026-11-07 18:25:07");
    }

    #[test]
    fn a_report_says_what_where_and_which_version_and_the_newest_is_found() {
        let folder = std::env::temp_dir().join(format!("dicing_splicing_crash_{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        assert_eq!(newest_report(&folder), None);
        let text = report("index out of bounds", "src/ui/timeline.rs:10", "main", "0: main", 0);
        assert!(text.contains(VERSION) && text.contains("index out of bounds") && text.contains("src/ui/timeline.rs:10"), "{text}");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("crash-2026-11-01-10-00-00-1.txt"), "old").unwrap();
        fs::write(folder.join("notes.txt"), "not a report").unwrap();
        let written = write_report(&folder, &text).unwrap();
        assert_eq!(newest_report(&folder), Some(written.clone()));
        assert_eq!(fs::read_to_string(written).unwrap(), text);
        let _ = fs::remove_dir_all(&folder);
    }
}
