//! `mm:ss:ff` timecodes: minutes, seconds and the frame within the second.

/// Index of the frame shown at `t`.
pub fn frame_index(t: f64, fps: f64) -> i64 {
    (t * fps + 1e-6).floor() as i64
}

pub fn format(t: f64, fps: f64) -> String {
    let frame_start = frame_index(t.max(0.0), fps) as f64 / fps;
    let seconds = (frame_start + 1e-9).floor();
    let frame = frame_index(frame_start - seconds, fps);
    let seconds = seconds as u64;
    format!("{:02}:{:02}:{:02}", seconds / 60, seconds % 60, frame)
}

/// `mm:ss`, for ruler labels on whole seconds.
pub fn format_minutes(t: f64) -> String {
    let seconds = t.max(0.0).round() as u64;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// Parses `mm:ss:ff`, `mm:ss` (seconds may be fractional) or plain seconds.
pub fn parse(text: &str, fps: f64) -> Option<f64> {
    let number = |s: &str| s.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0);
    match text.trim().split(':').collect::<Vec<_>>().as_slice() {
        [s] => number(s),
        [m, s] => Some(number(m)? * 60.0 + number(s)?),
        [m, s, f] => Some(number(m)? * 60.0 + number(s)? + number(f)? / fps),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_frames_within_the_second() {
        assert_eq!(format(0.0, 30.0), "00:00:00");
        assert_eq!(format(1.0, 30.0), "00:01:00");
        assert_eq!(format(65.5, 30.0), "01:05:15");
        assert_eq!(format(65.49, 30.0), "01:05:14");
        // 29.97 fps: frame 29 starts before the 1 s mark, frame 30 after it.
        assert_eq!(format(1.0, 30000.0 / 1001.0), "00:00:29");
    }

    #[test]
    fn parses_what_it_formats() {
        assert_eq!(parse("01:05:15", 30.0), Some(65.5));
        assert_eq!(parse("1:05", 30.0), Some(65.0));
        assert_eq!(parse("12.5", 30.0), Some(12.5));
        assert_eq!(parse("2:03.25", 30.0), Some(123.25));
        assert_eq!(parse("abc", 30.0), None);
        assert_eq!(parse("1:2:3:4", 30.0), None);
        assert_eq!(parse("-3", 30.0), None);
    }
}
