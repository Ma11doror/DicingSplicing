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

pub fn update_preview(project: Project) {
    let data = project.data.lock().unwrap().clone();
    let preview_store = project.preview_frame.clone();

    if let Some(source) = data.source_path {
        tokio::spawn(async move {
            // Extract single frame via ffmpeg to stdout pipe
            let output = Command::new("ffmpeg")
                .arg("-y")
                .arg("-ss")
                .arg(data.start_time.to_string())
                .arg("-i")
                .arg(&source)
                .arg("-vframes")
                .arg("1")
                .arg("-f")
                .arg("image2pipe")
                .arg("-c:v")
                .arg("mjpeg")
                .arg("-")
                .output();

            match output {
                Ok(o) if o.status.success() => {
                    // Decode JPEG to raw pixels for Egui
                    if let Ok(img) =
                        image::load_from_memory_with_format(&o.stdout, image::ImageFormat::Jpeg)
                    {
                        let size = [img.width() as usize, img.height() as usize];
                        let image_buffer = img.to_rgba8();
                        let pixels = image_buffer.as_flat_samples();

                        let color_image =
                            egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_slice());

                        let mut p = preview_store.lock().unwrap();
                        *p = Some(color_image);
                    }
                }
                _ => {
                    println!("[Preview] Failed to extract frame");
                }
            }
        });
    }
}

pub fn start_processing(project: Project) {
    let status = project.status.clone();
    let data = project.data.clone();

    tokio::spawn(async move {
        let (source_path, start_time, duration, crop_x, crop_y, crop_zoom) = {
            let d = data.lock().unwrap();
            if d.source_path.is_none() {
                // error handling...
                return;
            }
            (
                d.source_path.clone().unwrap(),
                d.start_time,
                d.duration,
                d.crop_x,
                d.crop_y,
                d.crop_zoom,
            )
        };

        {
            let mut s = status.lock().unwrap();
            *s = ProjectStatus::Processing;
        }

        // Логика кропа для FFmpeg
        // crop=w:h:x:y
        // Нам нужно перевести относительные координаты GUI (0.0 center) в пиксели FFmpeg

        // ВАЖНО: Это упрощенная логика.
        // Для точного кропа нам нужно знать размер видео.
        // Пока используем центрированный кроп с зумом:
        // scale=400*zoom:-1, crop=400:400

        // Более точный вариант:
        // Сначала вырезаем квадрат, потом скейлим.
        // Но FFmpeg фильтры сложны.
        // Используем трюк: скейлим видео так, чтобы нужная область стала центром, потом кропаем.

        let output_path = source_path.replace(".mp4", "_circle.mp4");

        // Формула кропа:
        // 1. Приближаем видео (scale) согласно зуму.
        // 2. Смещаем (crop x/y).

        // Для надежности пока оставим простой Center Crop + Scale 400x400
        // (Реализация точного маппинга координат Egui -> FFmpeg требует получения разрешения видео через ffprobe,
        //  что добавит еще шаг. Сейчас сделаем базовый вариант).

        let filter = "crop='min(iw,ih)':'min(iw,ih)',scale=400:400"; // Placeholder пока не сделаем ffprobe

        let output = Command::new("ffmpeg")
            .arg("-y")
            .arg("-ss")
            .arg(start_time.to_string())
            .arg("-t")
            .arg(duration.to_string())
            .arg("-i")
            .arg(&source_path)
            .arg("-vf")
            .arg(filter)
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

        // ... (обработка результата такая же как была)
        let mut s = status.lock().unwrap();
        if let Ok(o) = output {
            if o.status.success() {
                *s = ProjectStatus::Done(output_path);
            } else {
                *s = ProjectStatus::Error(String::from_utf8_lossy(&o.stderr).to_string());
            }
        }
    });
}
