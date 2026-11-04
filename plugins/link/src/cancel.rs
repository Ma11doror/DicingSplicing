//! Stopping what is running, with everything it started.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct Cancel {
    cancelled: Arc<AtomicBool>,
    /// The program running now.
    running: Arc<Mutex<Option<u32>>>,
}

impl Cancel {
    pub fn trigger(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(pid) = self.running.lock().ok().and_then(|running| *running) {
            kill_tree(pid);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Tells what to stop from now on; a cancel that came just before this still reaches it.
    pub fn started(&self, pid: u32) {
        if let Ok(mut running) = self.running.lock() {
            *running = Some(pid);
        }
        if self.is_cancelled() {
            kill_tree(pid);
        }
    }

    pub fn finished(&self) {
        if let Ok(mut running) = self.running.lock() {
            *running = None;
        }
    }
}

/// yt-dlp is several processes (the packed Windows build is two by itself, and it runs ffmpeg),
/// so ending only the one we started would leave the download going.
fn kill_tree(pid: u32) {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("taskkill");
        command.args(["/T", "/F", "/PID", &pid.to_string()]);
        command
    } else {
        let mut command = Command::new("kill");
        command.arg(pid.to_string());
        command
    };
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    hide_window(&mut command);
    let _ = command.status();
}

/// No console window flashing for a program that runs in the background.
pub fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}
