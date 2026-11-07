//! Running external tools (ffmpeg, ffprobe).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{OnceLock, RwLock};

pub const FFMPEG: &str = "ffmpeg";
pub const FFPROBE: &str = "ffprobe";

/// Programs that are not called by their plain name: (name, path).
static PROGRAMS: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());

/// Sets where a program is. An empty path means looking for it on the PATH.
pub fn set_program(name: &str, path: &str) {
    if let Ok(mut programs) = PROGRAMS.write() {
        programs.retain(|(known, _)| known != name);
        if !path.trim().is_empty() {
            programs.push((name.to_owned(), path.trim().to_owned()));
        }
    }
}

/// Where a program is: as the settings say; else the one in the editor's own folder, which is where
/// a packaged editor brings its ffmpeg; else its name, for the PATH.
pub fn program_path(name: &str) -> String {
    let programs = PROGRAMS.read();
    let known = programs.as_ref().ok().and_then(|programs| programs.iter().find(|(known, _)| known == name).map(|(_, path)| path.clone()));
    known.or_else(|| editor_folder().and_then(|folder| beside(folder, name))).unwrap_or_else(|| name.to_owned())
}

/// The folder the editor runs from.
fn editor_folder() -> Option<&'static Path> {
    static FOLDER: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOLDER.get_or_init(|| std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_owned))).as_deref()
}

/// The program `name` in `folder`, if it is there.
fn beside(folder: &Path, name: &str) -> Option<String> {
    let file = folder.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() });
    file.is_file().then(|| file.to_string_lossy().into_owned())
}

/// A command for an external tool. On Windows it never opens a console window,
/// which would otherwise flash for every call in release builds.
pub fn command(program: &str) -> Command {
    let mut cmd = Command::new(program_path(program));
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Stops a program with everything it started. What is found on the PATH may be only a launcher
/// that waits for the real program (Chocolatey's `ffmpeg.exe` is one): ending just the launcher
/// would leave the work going, and it would be holding the pipes we read.
pub fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        // It finds the rest through the parent, so the parent has to be alive at this moment.
        let pid = child.id().to_string();
        let stopped = command("taskkill").args(["/T", "/F", "/PID", &pid]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        if stopped.is_ok_and(|status| status.success()) {
            return;
        }
    }
    let _ = child.kill();
}

/// Checks that a tool runs and returns the first line of its version output.
pub fn check(program: &str) -> Result<String> {
    let output = command(program)
        .arg("-version")
        .output()
        .with_context(|| format!("{program} was not found: install it and add it to PATH, or say where it is in Settings"))?;
    if !output.status.success() {
        bail!("{program} -version failed");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().next().unwrap_or_default().to_string())
}

/// The encoders of the installed ffmpeg: builds differ. Empty if it can't be asked.
pub fn encoders() -> Vec<String> {
    let output = command(FFMPEG).args(["-hide_banner", "-encoders"]).output();
    output.map_or_else(|_| Vec::new(), |output| parse_encoders(&String::from_utf8_lossy(&output.stdout)))
}

/// After a legend that ends with a line of dashes, each line is ` V....D name  description`.
fn parse_encoders(text: &str) -> Vec<String> {
    let lines = text.lines().skip_while(|line| !line.trim_start().starts_with("---")).skip(1);
    lines.filter_map(|line| line.split_whitespace().nth(1)).map(str::to_owned).collect()
}

/// Opens a file in the program the system has for it.
pub fn open_file(path: &Path) -> Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = command("explorer");
        cmd.arg(path);
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = command("xdg-open");
        cmd.arg(path);
        cmd
    };
    cmd.spawn().context("Can't open the file")?;
    Ok(())
}

/// Shows a file in the file manager, selected if the system can do that.
pub fn reveal_file(path: &Path) -> Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        // Explorer wants the quotes around the path only, which a normal argument can't do.
        let mut cmd = command("explorer");
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = command("xdg-open");
        cmd.arg(path.parent().unwrap_or(path));
        cmd
    };
    cmd.spawn().context("Can't show the file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_path_is_used_until_it_is_cleared() {
        assert_eq!(program_path("some_tool"), "some_tool");
        set_program("some_tool", " C:/bin/some_tool.exe ");
        assert_eq!(program_path("some_tool"), "C:/bin/some_tool.exe");
        set_program("some_tool", "");
        assert_eq!(program_path("some_tool"), "some_tool");
    }

    #[test]
    fn a_program_in_the_folder_is_found_by_its_file() {
        let folder = std::env::temp_dir().join(format!("simple_editor_tools_{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        assert_eq!(beside(&folder, "some_tool"), None);
        let file = folder.join(if cfg!(windows) { "some_tool.exe" } else { "some_tool" });
        std::fs::write(&file, b"").unwrap();
        assert_eq!(beside(&folder, "some_tool"), Some(file.to_string_lossy().into_owned()));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[cfg(windows)]
    #[test]
    fn stopping_a_program_stops_what_it_started() {
        use std::io::Read;
        use std::time::{Duration, Instant};

        // `cmd` waits for `ping` the way a launcher waits for the real program, and `ping` holds the pipe.
        let mut child = command("cmd").args(["/c", "ping -n 60 127.0.0.1"]).stdout(Stdio::piped()).spawn().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        stdout.read_exact(&mut [0; 1]).unwrap();

        let started = Instant::now();
        kill_tree(&mut child);
        // This ends only when nothing holds the pipe anymore.
        stdout.read_to_end(&mut Vec::new()).unwrap();
        let _ = child.wait();
        assert!(started.elapsed() < Duration::from_secs(10), "it took {:?}", started.elapsed());
    }

    #[test]
    fn finds_the_encoders_after_the_legend() {
        let text = "Encoders:\n V..... = Video\n A..... = Audio\n ------\n V....D libx264              H.264\n A....D aac                  AAC\n";
        assert_eq!(parse_encoders(text), ["libx264", "aac"]);
    }
}
