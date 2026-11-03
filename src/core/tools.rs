//! Running external tools (ffmpeg, ffprobe).

use anyhow::{bail, Context, Result};
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
