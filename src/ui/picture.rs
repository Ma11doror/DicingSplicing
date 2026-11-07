//! What a view of the editor shows: a frame of whichever source it needs, decoded from the frames
//! prepared for the editor. Each source gets a decoder of its own when it is first needed, so that
//! going back and forth between clips of two sources does not start anything anew.

use crate::core::edit::{Color, SourceId};
use crate::media::cache::MediaCache;
use crate::media::color::Grade;
use crate::media::frames::FrameDecoder;
use crate::media::{timecode, Notify};
use egui::{ColorImage, TextureHandle, TextureOptions};
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
    /// The last frame as it was decoded, for its colour to change without decoding it again.
    raw: Option<ColorImage>,
    /// The colour frames are shown in, and the one the texture has.
    color: Option<Color>,
    shown_color: Option<Color>,
}

impl Picture {
    pub fn new(name: &'static str, notify: Notify) -> Self {
        Self { name, notify, decoders: BTreeMap::new(), texture: None, shown: None, wanted: None, raw: None, color: None, shown_color: None }
    }

    /// The colour of the clip the frames are of; None shows them as they are.
    pub fn set_color(&mut self, color: Option<Color>) {
        self.color = color;
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

    /// Takes in what the decoders have decoded, in the colour asked for. Frames of a source no longer
    /// wanted are dropped. A new colour redraws the frame there is.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let wanted = self.wanted.map(|(source, _)| source);
        let mut fresh = false;
        for (&source, (_, decoder)) in &self.decoders {
            let Some(image) = decoder.poll() else { continue };
            if wanted != Some(source) {
                continue;
            }
            self.raw = Some(image);
            self.shown = Some(source);
            fresh = true;
        }
        if !fresh && self.color == self.shown_color {
            return;
        }
        let Some(raw) = &self.raw else { return };
        let mut image = raw.clone();
        if let Some(color) = &self.color {
            Grade::new(color).apply(&mut image.pixels);
        }
        match &mut self.texture {
            Some(texture) => texture.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture(self.name, image, TextureOptions::LINEAR)),
        }
        self.shown_color = self.color;
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
            self.raw = None;
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
