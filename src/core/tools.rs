//! Running external tools (ffmpeg, ffprobe).

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

pub const FFMPEG: &str = "ffmpeg";
pub const FFPROBE: &str = "ffprobe";

/// A command for an external tool. On Windows it never opens a console window,
/// which would otherwise flash for every call in release builds.
pub fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Checks that a tool runs and returns the first line of its version output.
pub fn check(program: &str) -> Result<String> {
    let output = command(program)
        .arg("-version")
        .output()
        .with_context(|| format!("{program} was not found: install it and add it to PATH"))?;
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
    fn finds_the_encoders_after_the_legend() {
        let text = "Encoders:\n V..... = Video\n A..... = Audio\n ------\n V....D libx264              H.264\n A....D aac                  AAC\n";
        assert_eq!(parse_encoders(text), ["libx264", "aac"]);
    }
}
