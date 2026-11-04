//! The message that goes before the circle: text and a link are kept apart, and only here become
//! MarkdownV2, so that nothing the user types can break the markup.

use anyhow::{bail, Result};
use std::collections::BTreeMap;

/// Everything MarkdownV2 gives a meaning to. The old script left out the backslash.
const SPECIAL: &str = "\\_*[]()~`>#+-=|{}.!";

/// What the video is called in the channel, from what is known about it.
pub fn default_text(meta: &BTreeMap<String, String>, project: &str) -> String {
    let known = |key: &str| meta.get(key).map(|value| value.trim()).filter(|value| !value.is_empty());
    match (known("title"), known("artist")) {
        (Some(title), Some(artist)) => format!("\"{title}\" by {artist}"),
        (Some(title), None) => format!("\"{title}\""),
        (None, Some(artist)) => format!("Unknown Song by {artist}"),
        // A link should say something. Without one, nothing goes unless the user writes it.
        (None, None) if known("link").is_some() => project.trim().to_owned(),
        (None, None) => String::new(),
    }
}

pub fn default_link(meta: &BTreeMap<String, String>) -> String {
    meta.get("link").map_or_else(String::new, |link| link.trim().to_owned())
}

fn escape_chars(text: &str, special: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if special.contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

fn escape(text: &str) -> String {
    escape_chars(text, SPECIAL)
}

/// Inside a link's address, only these two mean something.
fn escape_address(link: &str) -> String {
    escape_chars(link, "\\)")
}

/// The message to send, or none if there is nothing to say.
pub fn message(text: &str, link: &str) -> Result<Option<String>> {
    let (text, link) = (text.trim(), link.trim());
    if !link.is_empty() && (!(link.starts_with("http://") || link.starts_with("https://")) || link.contains(char::is_whitespace)) {
        bail!("The link should start with http:// or https:// and have no spaces");
    }
    Ok(match (text.is_empty(), link.is_empty()) {
        (true, true) => None,
        (false, true) => Some(escape(text)),
        (true, false) => Some(format!("[{}]({})", escape(link), escape_address(link))),
        (false, false) => Some(format!("[{}]({})", escape(text), escape_address(link))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
    }

    #[test]
    fn the_text_is_made_the_way_the_script_made_it() {
        let link = ("link", "https://song.link/x");
        assert_eq!(default_text(&meta(&[("title", "T"), ("artist", "A"), link]), "p"), "\"T\" by A");
        assert_eq!(default_text(&meta(&[("title", "T"), link]), "p"), "\"T\"");
        assert_eq!(default_text(&meta(&[("artist", "A"), link]), "p"), "Unknown Song by A");
    }

    #[test]
    fn a_project_name_is_used_only_as_a_links_text() {
        assert_eq!(default_text(&meta(&[("link", "https://youtu.be/x")]), "Video title"), "Video title");
        // A local file: its project is probably called something only its author should see.
        assert_eq!(default_text(&meta(&[]), "final_v3_really"), "");
        assert_eq!(default_text(&meta(&[("title", "  ")]), "p"), "");
    }

    #[test]
    fn the_link_comes_from_the_download() {
        assert_eq!(default_link(&meta(&[("link", " https://song.link/x "), ("video", "https://youtu.be/x")])), "https://song.link/x");
        assert_eq!(default_link(&meta(&[("video", "https://youtu.be/x")])), "");
    }

    #[test]
    fn nothing_typed_can_break_the_markup() {
        let every_special = "_*[]()~`>#+-=|{}.!\\";
        let escaped = message(every_special, "").unwrap().unwrap();
        assert_eq!(escaped, "\\_\\*\\[\\]\\(\\)\\~\\`\\>\\#\\+\\-\\=\\|\\{\\}\\.\\!\\\\");
        assert_eq!(message("Песня (live) - 1.0", "").unwrap().unwrap(), "Песня \\(live\\) \\- 1\\.0");
    }

    #[test]
    fn text_and_link_make_a_link() {
        let made = message(" \"A.B\" by C ", " https://song.link/x_1 ").unwrap().unwrap();
        assert_eq!(made, "[\"A\\.B\" by C](https://song.link/x_1)");
        // In the address only what would end it is escaped, or the link would change.
        let made = message("t", "https://a.b/x_(1)\\y").unwrap().unwrap();
        assert_eq!(made, "[t](https://a.b/x_(1\\)\\\\y)");
        // A link alone is its own text.
        assert_eq!(message("", "https://a.b/c").unwrap().unwrap(), "[https://a\\.b/c](https://a.b/c)");
    }

    #[test]
    fn empty_fields_mean_no_message() {
        assert_eq!(message("", "").unwrap(), None);
        assert_eq!(message("  ", "\n").unwrap(), None);
    }

    #[test]
    fn only_web_links_are_accepted() {
        for bad in ["song.link/x", "tg://resolve?domain=x", "javascript:x", "https://a b"] {
            assert!(message("t", bad).is_err(), "{bad}");
        }
    }
}
