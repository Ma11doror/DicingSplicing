use crate::core::project::{Project, ProjectStatus};
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn start_download(project: Project) {
    let status = project.status.clone();
    let data = project.data.clone();

    tokio::spawn(async move {
        // 1. Set status
        {
            let mut s = status.lock().unwrap();
            *s = ProjectStatus::Downloading(0.0);
        }

        let url = { data.lock().unwrap().url.clone() };
        let output_dir = "downloads";
        let _ = fs::create_dir_all(output_dir);

        // Define the "Nice Name" template
        // We force .mp4 extension in the name to match the merge format
        let name_template = format!("{}/%(title)s.mp4", output_dir);

        println!("[Task] Step 1: Resolving filename for {}", url);

        // STEP 1: Get the sanitized filename from yt-dlp (dry-run)
        let name_output = Command::new("yt-dlp")
            .arg("--print")
            .arg("filename")
            .arg("-o")
            .arg(&name_template)
            .arg("--no-playlist")
            .arg(&url)
            .output();

        let target_path_str = match name_output {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout);
                s.trim().to_string() // Removes newline characters
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                let mut s = status.lock().unwrap();
                *s = ProjectStatus::Error(format!("Failed to resolve name: {}", err));
                return;
            }
            Err(e) => {
                let mut s = status.lock().unwrap();
                *s = ProjectStatus::Error(format!("Process error: {}", e));
                return;
            }
        };

        if target_path_str.is_empty() {
            let mut s = status.lock().unwrap();
            *s = ProjectStatus::Error("yt-dlp returned empty filename".to_string());
            return;
        }

        println!("[Task] Step 2: Downloading to '{}'", target_path_str);

        // STEP 2: Actual Download using the resolved path
        let output = Command::new("yt-dlp")
            .arg("-f")
            .arg("bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best")
            .arg("--merge-output-format")
            .arg("mp4")
            .arg("-o")
            .arg(&target_path_str) // Use the specific path we resolved
            .arg("--no-playlist")
            .arg("--verbose")
            .arg(&url)
            .output();

        // 3. Handle Result
        let mut s = status.lock().unwrap();
        match output {
            Ok(o) => {
                // Log output for debugging
                println!("[yt-dlp STDOUT]:\n{}", String::from_utf8_lossy(&o.stdout));

                if o.status.success() {
                    if Path::new(&target_path_str).exists() {
                        let mut d = data.lock().unwrap();
                        d.source_path = Some(target_path_str.clone());
                        d.filename_base = Path::new(&target_path_str)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();

                        *s = ProjectStatus::Ready;
                        println!("[Task] Download Complete: {}", target_path_str);
                    } else {
                        *s = ProjectStatus::Error("Download success but file missing".to_string());
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    println!("[yt-dlp STDERR]:\n{}", stderr);
                    *s = ProjectStatus::Error(format!("Download failed. Check logs."));
                }
            }
            Err(e) => {
                *s = ProjectStatus::Error(format!("Failed to execute download: {}", e));
            }
        }
    });
}

pub fn start_processing(project: Project) {
    let status = project.status.clone();
    let data = project.data.clone();

    tokio::spawn(async move {
        // 1. Validate state
        let (source_path, start_time, duration) = {
            let d = data.lock().unwrap();
            if d.source_path.is_none() {
                let mut s = status.lock().unwrap();
                *s = ProjectStatus::Error("No source file to process".to_string());
                return;
            }
            (d.source_path.clone().unwrap(), d.start_time, d.duration)
        };

        // 2. Set Status
        {
            let mut s = status.lock().unwrap();
            *s = ProjectStatus::Processing;
        }

        println!("[Task] Starting processing...");
        println!("[Task] Source: {}", source_path);
        println!("[Task] Cut: {}s to {}s", start_time, start_time + duration);

        // 3. Prepare Output Path
        // Input: "downloads/Song.mp4" -> Output: "downloads/Song_circle.mp4"
        let output_path = source_path.replace(".mp4", "_circle.mp4");

        // 4. Construct FFmpeg Command
        // We perform:
        // -ss / -t: Precise cutting
        // -vf: Crop to square (center) and Scale to 400x400 (Standard TG Circle)
        // -c:v libx264: Re-encode for compatibility
        let output = Command::new("ffmpeg")
            .arg("-y") // Overwrite output
            .arg("-ss")
            .arg(start_time.to_string()) // Seek to start
            .arg("-t")
            .arg(duration.to_string()) // Duration
            .arg("-i")
            .arg(&source_path) // Input
            .arg("-vf")
            .arg("crop='min(iw,ih)':'min(iw,ih)',scale=400:400") // Center Square Crop + Resize
            .arg("-c:v")
            .arg("libx264")
            .arg("-preset")
            .arg("medium")
            .arg("-c:a")
            .arg("aac")
            .arg("-b:a")
            .arg("128k")
            .arg(&output_path)
            .output();

        // 5. Handle Result
        let mut s = status.lock().unwrap();
        match output {
            Ok(o) => {
                if o.status.success() {
                    if Path::new(&output_path).exists() {
                        *s = ProjectStatus::Done(output_path);
                        println!("[Task] Processing Complete.");
                    } else {
                        *s = ProjectStatus::Error("FFmpeg success but file missing".to_string());
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    println!("[FFmpeg STDERR]:\n{}", stderr);
                    *s = ProjectStatus::Error(format!("FFmpeg failed: check terminal"));
                }
            }
            Err(e) => {
                *s = ProjectStatus::Error(format!("Failed to run ffmpeg: {}", e));
            }
        }
    });
}
