//! Decodes cached frames on a worker thread. Only the latest request matters: while scrubbing,
//! frames the UI has already moved past are skipped instead of queued.

use crate::media::cache::FrameIndex;
use crate::media::Notify;
use image::ImageFormat;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct FrameDecoder {
    shared: Arc<Shared>,
    results: Receiver<egui::ColorImage>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Request {
    frame: Option<usize>,
    shutdown: bool,
}

#[derive(Default)]
struct Shared {
    request: Mutex<Request>,
    wake: Condvar,
}

impl FrameDecoder {
    pub fn new(path: PathBuf, index: Arc<FrameIndex>, notify: Notify) -> Self {
        let shared = Arc::new(Shared::default());
        let (sender, results) = mpsc::channel();
        let worker = {
            let shared = shared.clone();
            thread::spawn(move || decode_loop(&path, &index, &shared, &sender, &notify))
        };
        Self { shared, results, worker: Some(worker) }
    }

    /// Asks for a frame, replacing any request that has not been picked up yet.
    pub fn request(&self, frame: usize) {
        self.shared.request.lock().unwrap().frame = Some(frame);
        self.shared.wake.notify_one();
    }

    /// The newest decoded frame since the last call, if any.
    pub fn poll(&self) -> Option<egui::ColorImage> {
        self.results.try_iter().last()
    }
}

impl Drop for FrameDecoder {
    fn drop(&mut self) {
        self.shared.request.lock().unwrap().shutdown = true;
        self.shared.wake.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn decode_loop(path: &Path, index: &FrameIndex, shared: &Shared, results: &Sender<egui::ColorImage>, notify: &Notify) {
    let mut file: Option<File> = None;
    let mut jpeg = Vec::new();
    loop {
        let (offset, len) = {
            let mut request = shared.request.lock().unwrap();
            loop {
                if request.shutdown {
                    return;
                }
                match request.frame {
                    Some(frame) => match index.get(frame) {
                        Some((offset, len)) => {
                            request.frame = None;
                            break (offset, len);
                        }
                        // Not cached yet: keep it requested and look again shortly.
                        None => request = shared.wake.wait_timeout(request, Duration::from_millis(100)).unwrap().0,
                    },
                    None => request = shared.wake.wait(request).unwrap(),
                }
            }
        };

        if file.is_none() {
            file = File::open(path).ok();
        }
        let Some(f) = file.as_mut() else { continue };
        jpeg.resize(len as usize, 0);
        if f.seek(SeekFrom::Start(offset)).and_then(|_| f.read_exact(&mut jpeg)).is_err() {
            file = None;
            continue;
        }
        let Ok(decoded) = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg) else { continue };
        let rgb = decoded.into_rgb8();
        let image = egui::ColorImage::from_rgb([rgb.width() as usize, rgb.height() as usize], rgb.as_raw());
        if results.send(image).is_err() {
            return;
        }
        notify();
    }
}
