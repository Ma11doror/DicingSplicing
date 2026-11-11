# Changelog

## v0.1.0 — 2026-11-11

The first release.

**Editing**

- Projects of several videos on several tracks: the top visible track is seen, the sound of all of
  them is mixed.
- Videos from files, dropped on the window, or by a link (YouTube, song.link and anything yt-dlp
  understands). Videos are linked, not copied.
- Move clips along and between tracks, trim them, cut them with the Blade or at the playhead,
  duplicate, copy and paste, delete with or without closing the gap. Undo and redo.
- Unfolded tracks show each clip's effects and the graph of its sound.
- The timeline and the side panels can be resized; tracks scroll a little past their ends and
  spring back.

**Framing**

- Frames for Telegram circles, Reels, Shorts and TikTok, landscape, tablet, photo, classic, square,
  Instagram post, cinema, or a ratio of your own; several can be shown at once.
- Move, resize and rotate the frame on the picture; mirror; fill or black bars; thirds, center and
  safe-area guides.

**Effects**

- Fade in and fade out of the picture and the sound, linear or by one of six easing curves; a fading
  clip reveals the clip under it.
- Color: exposure, contrast, saturation, temperature.

**Export**

- Presets for Telegram circles, Reels, Shorts and TikTok, YouTube and Instagram posts; MP4 (H.264),
  WebM (VP9), MOV, MKV; AAC or Opus.
- Sound fades at the edges of the file, even loudness (-14 LUFS), saving a single frame.
- Send to Telegram as a circle, through your own bot.

**The program**

- Runs from its folder: FFmpeg and yt-dlp come along.
- Error reports in `%APPDATA%\DicingSplicing\logs`, and a list of keys on F1.

**Known limitations**

- Tested on Windows 11 only.
- The program is not signed: Windows asks to confirm the first start.
- Sending to Telegram needs your own bot and a `credentials.json` written by hand.
- Sound can't yet be separated from its video, and sound files can't yet be added: that is next.
