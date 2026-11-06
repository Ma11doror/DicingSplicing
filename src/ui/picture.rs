//! What a view of the editor shows: a frame of whichever source it needs, decoded from the frames
//! prepared for the editor. Each source gets a decoder of its own when it is first needed, so that
//! going back and forth between clips of two sources does not start anything anew.

use crate::core::edit::SourceId;
use crate::media::cache::MediaCache;
use crate::media::frames::FrameDecoder;
use crate::media::{timecode, Notify};
use egui::{TextureHandle, TextureOptions};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct Picture {
    name: &'static str,
    notify: Notify,
    /// With the frames file each one reads: a file put in another's place has new frames.
    decoders: BTreeMap<SourceId, (PathBuf, FrameDecoder)>,
    texture: Option<TextureHandle>,
    /// The source whose frame the texture holds.
    shown: Option<SourceId>,
    /// The frame asked for last.
    wanted: Option<(SourceId, usize)>,
}

impl Picture {
    pub fn new(name: &'static str, notify: Notify) -> Self {
        Self { name, notify, decoders: BTreeMap::new(), texture: None, shown: None, wanted: None }
    }

    /// Asks for the frame of `source` at source time `t`. Returns the frame's number.
    pub fn want(&mut self, source: SourceId, t: f64, cache: &MediaCache) -> usize {
        let frame = frame_at(cache, t);
        if self.wanted == Some((source, frame)) {
            return frame;
        }
        let path = cache.frames_path();
        if self.decoders.get(&source).is_none_or(|(known, _)| *known != path) {
            let decoder = FrameDecoder::new(path.clone(), cache.frames.clone(), self.notify.clone());
            self.decoders.insert(source, (path, decoder));
        }
        if let Some((_, decoder)) = self.decoders.get(&source) {
            decoder.request(frame);
        }
        self.wanted = Some((source, frame));
        frame
    }

    /// Takes in what the decoders have decoded. Frames of a source no longer wanted are dropped.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let wanted = self.wanted.map(|(source, _)| source);
        for (&source, (_, decoder)) in &self.decoders {
            let Some(image) = decoder.poll() else { continue };
            if wanted != Some(source) {
                continue;
            }
            match &mut self.texture {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture(self.name, image, TextureOptions::LINEAR)),
            }
            self.shown = Some(source);
        }
    }

    /// The texture, if it holds a frame of `source`.
    pub fn of(&self, source: SourceId) -> Option<&TextureHandle> {
        self.texture.as_ref().filter(|_| self.shown == Some(source))
    }

    /// Forgets a source, whose frames are gone or are being made again.
    pub fn forget(&mut self, source: SourceId) {
        self.decoders.remove(&source);
        if self.shown == Some(source) {
            self.shown = None;
        }
        if self.wanted.is_some_and(|(wanted, _)| wanted == source) {
            self.wanted = None;
        }
    }
}

/// The prepared frame at source time `t`.
pub fn frame_at(cache: &MediaCache, t: f64) -> usize {
    let last = cache.frame_count() as i64 - 1;
    timecode::frame_index(t, cache.format.fps.as_f64()).clamp(0, last.max(0)) as usize
}
