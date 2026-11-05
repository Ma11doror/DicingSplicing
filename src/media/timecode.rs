//! Times as the user reads and types them: `mm:ss.cc`, minutes, seconds and hundredths.
//! Time counted from the export start can be negative.

/// Index of the frame shown at `t`.
pub fn frame_index(t: f64, fps: f64) -> i64 {
    (t * fps + 1e-6).floor() as i64
}

pub fn format(t: f64) -> String {
    let hundredths = (t.abs() * 100.0).round() as u64;
    let seconds = hundredths / 100;
    format!("{}{:02}:{:02}.{:02}", sign(t, hundredths), seconds / 60, seconds % 60, hundredths % 100)
}

/// `mm:ss`, for ruler labels on whole seconds.
pub fn format_minutes(t: f64) -> String {
    let seconds = t.abs().round() as u64;
    format!("{}{:02}:{:02}", sign(t, seconds), seconds / 60, seconds % 60)
}

/// What goes before a number that `t` was rounded to: zero has no sign.
fn sign(t: f64, rounded: u64) -> &'static str {
    if t < 0.0 && rounded > 0 {
        "-"
    } else {
        ""
    }
}

/// Parses `mm:ss.cc`, `mm:ss:ff` or plain seconds, with a minus in front if it is before the zero.
pub fn parse(text: &str, fps: f64) -> Option<f64> {
    let text = text.trim();
    let (sign, text) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text),
    };
    let number = |s: &str| s.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0);
    let magnitude = match text.split(':').collect::<Vec<_>>().as_slice() {
        [s] => number(s),
        [m, s] => Some(number(m)? * 60.0 + number(s)?),
        [m, s, f] => Some(number(m)? * 60.0 + number(s)? + number(f)? / fps),
        _ => None,
    };
    magnitude.map(|magnitude| sign * magnitude)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_hundredths() {
        assert_eq!(format(0.0), "00:00.00");
        assert_eq!(format(1.0), "00:01.00");
        assert_eq!(format(65.5), "01:05.50");
        assert_eq!(format(120.0), "02:00.00");
        // Rounded as a whole, so that no 60 appears among the seconds.
        assert_eq!(format(59.999), "01:00.00");
    }

    #[test]
    fn formats_time_before_the_zero() {
        assert_eq!(format(-2.0), "-00:02.00");
        assert_eq!(format(-65.5), "-01:05.50");
        assert_eq!(format(-0.001), "00:00.00");
        assert_eq!(format_minutes(-5.0), "-00:05");
        assert_eq!(format_minutes(-0.2), "00:00");
        assert_eq!(format_minutes(125.0), "02:05");
    }

    #[test]
    fn counts_frames_before_the_zero_too() {
        assert_eq!(frame_index(-1.0, 30.0), -30);
        assert_eq!(frame_index(-0.01, 30.0), -1);
        assert_eq!(frame_index(0.0, 30.0), 0);
    }

    #[test]
    fn parses_what_it_formats() {
        for t in [0.0, 1.0, 65.5, 123.25, -2.0, -65.5] {
            assert_eq!(parse(&format(t), 30.0), Some(t));
        }
    }

    #[test]
    fn parses_what_people_type() {
        assert_eq!(parse("1:05", 30.0), Some(65.0));
        assert_eq!(parse("12.5", 30.0), Some(12.5));
        assert_eq!(parse("2:03.25", 30.0), Some(123.25));
        assert_eq!(parse(" -3 ", 30.0), Some(-3.0));
        assert_eq!(parse("-1:05", 30.0), Some(-65.0));
        // The old format still means what it meant.
        assert_eq!(parse("01:05:15", 30.0), Some(65.5));
        assert_eq!(parse("-00:01:15", 30.0), Some(-1.5));
    }

    #[test]
    fn refuses_what_is_not_a_time() {
        assert_eq!(parse("abc", 30.0), None);
        assert_eq!(parse("1:2:3:4", 30.0), None);
        assert_eq!(parse("--3", 30.0), None);
        assert_eq!(parse("1:-3", 30.0), None);
        assert_eq!(parse("", 30.0), None);
    }
}
