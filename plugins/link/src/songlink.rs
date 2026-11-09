//! Finding out what a song.link page is about. Everything here is best effort: when something is not
//! found, the link goes on with what is known. By now oEmbed mostly answers with nothing useful and
//! the public API is closed, so the page itself is what usually gives the name.

use crate::ytdlp::SEARCH_PREFIX;
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::time::Duration;

const OEMBED_URL: &str = "https://song.link/oembed";
const USER_AGENT: &str = concat!("dicing_splicing-link/", env!("CARGO_PKG_VERSION"));
const TIMEOUT: Duration = Duration::from_secs(20);
const HOSTS: [&str; 7] = ["song.link", "album.link", "artist.link", "playlist.link", "pods.link", "odesli.co", "mylink.page"];

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Song {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub youtube: Option<String>,
}

impl Song {
    /// What to give yt-dlp: the video if it is known, a search by the name if only that is known,
    /// or else the link as it is.
    pub fn download_target(&self, link: &str) -> String {
        if let Some(youtube) = &self.youtube {
            return youtube.clone();
        }
        match (&self.artist, &self.title) {
            (Some(artist), Some(title)) => format!("{SEARCH_PREFIX}{artist} {title}"),
            (None, Some(title)) => format!("{SEARCH_PREFIX}{title}"),
            _ => link.to_owned(),
        }
    }

    /// Takes what this one doesn't know yet from `other`.
    fn fill_from(&mut self, other: Song) {
        self.title = self.title.take().or(other.title);
        self.artist = self.artist.take().or(other.artist);
        self.youtube = self.youtube.take().or(other.youtube);
    }
}

pub fn is_song_link(link: &str) -> bool {
    let without_scheme = link.split_once("://").map_or(link, |(_, rest)| rest);
    let host = without_scheme.split(['/', '?', '#']).next().unwrap_or_default().to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    HOSTS.contains(&host)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).user_agent(USER_AGENT).build().into()
}

/// Everything song.link can tell about this link. If it can't, the song is just empty.
pub fn resolve(link: &str) -> Song {
    let mut song = fetch_oembed(link).unwrap_or_default();
    if song.youtube.is_none() || (song.title.is_none() && song.artist.is_none()) {
        if let Ok(from_page) = fetch_page(link) {
            song.fill_from(from_page);
        }
    }
    song
}

fn fetch_oembed(link: &str) -> Result<Song> {
    let mut response = agent().get(OEMBED_URL).query("url", link).query("format", "json").call().context("oEmbed request")?;
    parse_oembed(&response.body_mut().read_to_string()?)
}

fn fetch_page(link: &str) -> Result<Song> {
    let mut response = agent().get(link).call().context("page request")?;
    Ok(parse_page(&response.body_mut().read_to_string()?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Oembed {
    title: String,
    author_name: String,
    provider_url: String,
    html: String,
}

fn some_text(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// An author that is really the name of a service doesn't say who the artist is.
fn is_real_artist(author: &str) -> bool {
    let is_service = ["youtube", "soundcloud", "spotify"].iter().any(|service| author.eq_ignore_ascii_case(service));
    !author.is_empty() && !is_service && !author.contains("Topic")
}

fn parse_oembed(json: &str) -> Result<Song> {
    let oembed: Oembed = serde_json::from_str(json)?;
    let (raw_title, raw_author) = (oembed.title.trim(), oembed.author_name.trim());
    let (mut title, mut artist) = (None, None);

    if is_real_artist(raw_author) {
        artist = some_text(raw_author);
        if !raw_title.is_empty() {
            // Titles often repeat the artist: "Artist - Song".
            let without_artist = raw_title.replace(raw_author, "");
            let cleaned = without_artist.trim().trim_start_matches("- ").trim_end_matches(" -").trim();
            title = some_text(if cleaned.is_empty() { raw_title } else { cleaned });
        }
    } else if !raw_title.is_empty() {
        match raw_title.split_once(" - ") {
            Some((left, right)) => {
                artist = some_text(left);
                title = some_text(right);
            }
            None => title = some_text(raw_title),
        }
    }
    let artist = artist.map(|artist: String| artist.strip_suffix(" - Topic").map_or(artist.clone(), str::to_owned));

    let embed = Regex::new(r#"src="([^"]*youtube\.com[^"]*(?:embed/|watch\?v=)[a-zA-Z0-9_-]+[^"]*)""#).expect("the pattern is valid");
    let from_html = embed.captures(&oembed.html).map(|found| watch_url(&found[1]));
    let provider = &oembed.provider_url;
    let from_provider = (provider.contains("youtube.com") || provider.contains("youtu.be")).then(|| provider.clone());
    Ok(Song { title, artist, youtube: from_html.or(from_provider) })
}

/// An embedded player's address as the address of the video's page.
fn watch_url(url: &str) -> String {
    let id = Regex::new(r"(?:embed/|watch\?v=)([a-zA-Z0-9_-]+)").expect("the pattern is valid");
    match id.captures(url) {
        Some(found) => format!("https://www.youtube.com/watch?v={}", &found[1]),
        None => url.to_owned(),
    }
}

fn decode_entities(text: &str) -> String {
    let replacements = [("&#x27;", "'"), ("&#39;", "'"), ("&quot;", "\""), ("&lt;", "<"), ("&gt;", ">"), ("&amp;", "&")];
    replacements.iter().fold(text.to_owned(), |text, (entity, plain)| text.replace(entity, plain))
}

/// The content of a `<meta property="...">` tag, whichever way round its attributes are.
fn meta_content(html: &str, property: &str) -> Option<String> {
    let property = regex::escape(property);
    let patterns = [
        format!(r#"<meta[^>]*property="{property}"[^>]*content="([^"]*)""#),
        format!(r#"<meta[^>]*content="([^"]*)"[^>]*property="{property}""#),
    ];
    patterns.iter().find_map(|pattern| {
        let found = Regex::new(pattern).ok()?.captures(html)?;
        some_text(&decode_entities(&found[1]))
    })
}

/// A page title is "Song by Artist", sometimes "Artist - Song".
fn split_page_title(text: &str) -> (Option<String>, Option<String>) {
    if let Some((title, artist)) = text.split_once(" by ") {
        return (some_text(title), some_text(artist.trim_end_matches(" - Topic")));
    }
    if let Some((artist, title)) = text.split_once(" - ") {
        return (some_text(title), some_text(artist.trim_end_matches(" - Topic")));
    }
    (some_text(text), None)
}

fn parse_page(html: &str) -> Song {
    let (title, artist) = meta_content(html, "og:title").map_or((None, None), |text| split_page_title(&text));

    let meta_video = ["og:video:url", "og:video:secure_url"]
        .iter()
        .filter_map(|property| meta_content(html, property))
        .find(|url| url.contains("youtube.com") || url.contains("youtu.be"));
    let plain = || {
        let url = Regex::new(r"https?://(?:www\.)?(?:youtube\.com/watch\?v=|youtu\.be/)[a-zA-Z0-9_-]{11}").expect("the pattern is valid");
        url.find(html).map(|found| found.as_str().to_owned())
    };
    // The link is not always on the page, but the video's id may still be in its data.
    let in_data = || {
        [r"(?:youtube\|[a-z]+\||YOUTUBE_VIDEO::)([a-zA-Z0-9_-]{11})", r"i\.ytimg\.com/vi/([a-zA-Z0-9_-]{11})/"].iter().find_map(|pattern| {
            let found = Regex::new(pattern).ok()?.captures(html)?;
            Some(format!("https://www.youtube.com/watch?v={}", &found[1]))
        })
    };
    Song { title, artist, youtube: meta_video.or_else(plain).or_else(in_data) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Option<String> {
        Some(value.to_owned())
    }

    #[test]
    fn knows_the_links_it_can_ask_about() {
        assert!(is_song_link("https://song.link/s/abc"));
        assert!(is_song_link("https://WWW.Song.Link/s/abc?x=1"));
        assert!(is_song_link("https://album.link/i/1"));
        assert!(is_song_link("https://odesli.co/embed/?url=x"));
        assert!(!is_song_link("https://www.youtube.com/watch?v=abc"));
        assert!(!is_song_link("https://example.com/song.link"));
        assert!(!is_song_link("https://song.link.example.com/s/abc"));
    }

    /// What song.link really answered in November 2026: nothing about the song.
    #[test]
    fn the_answer_oembed_gives_now_says_nothing() {
        let json = r#"{"version":"1.0","type":"rich","provider_name":"Songlink","provider_url":"https://song.link","html":"<iframe width=\"560\" height=\"367\" src=\"https://odesli.co/embed/?url=https%3A%2F%2Fsong.link%2Fs%2F4uLU6hMCjMI75M1A2tKUQC\" frameborder=\"0\"></iframe>","width":560}"#;
        assert_eq!(parse_oembed(json).unwrap(), Song::default());
    }

    #[test]
    fn an_answer_with_details_gives_them() {
        let json = r#"{"title":"Rick Astley - Never Gonna Give You Up","author_name":"Rick Astley","html":"<iframe src=\"https://www.youtube.com/embed/dQw4w9WgXcQ?feature=oembed\"></iframe>"}"#;
        let song = parse_oembed(json).unwrap();
        assert_eq!(song.title, text("Never Gonna Give You Up"));
        assert_eq!(song.artist, text("Rick Astley"));
        assert_eq!(song.youtube, text("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
    }

    #[test]
    fn a_service_for_an_author_is_ignored() {
        let json = r#"{"title":"Some Band - A Song","author_name":"YouTube","provider_url":"https://youtu.be/dQw4w9WgXcQ"}"#;
        let song = parse_oembed(json).unwrap();
        assert_eq!(song.title, text("A Song"));
        assert_eq!(song.artist, text("Some Band"));
        assert_eq!(song.youtube, text("https://youtu.be/dQw4w9WgXcQ"));

        let json = r#"{"title":"A Song","author_name":"Some Band - Topic"}"#;
        let song = parse_oembed(json).unwrap();
        assert_eq!(song.title, text("A Song"));
        assert_eq!(song.artist, None);
    }

    /// The tag as it is on a real song.link page.
    #[test]
    fn the_page_gives_the_name() {
        let html = r#"<head><meta property="og:url" content="https://song.link/s/x"/><meta property="og:title" content="Never Gonna Give You Up by Rick Astley"/></head>"#;
        let song = parse_page(html);
        assert_eq!(song.title, text("Never Gonna Give You Up"));
        assert_eq!(song.artist, text("Rick Astley"));
        assert_eq!(song.youtube, None);
    }

    #[test]
    fn the_page_may_have_the_video_in_several_ways() {
        let tag = r#"<meta content="https://www.youtube.com/watch?v=aaaaaaaaaaa" property="og:video:url">"#;
        assert_eq!(parse_page(tag).youtube, text("https://www.youtube.com/watch?v=aaaaaaaaaaa"));

        let plain = r#"<a href="https://youtu.be/bbbbbbbbbbb">watch</a>"#;
        assert_eq!(parse_page(plain).youtube, text("https://youtu.be/bbbbbbbbbbb"));

        let data = r#"{"key":"YOUTUBE_VIDEO::ccccccccccc"}"#;
        assert_eq!(parse_page(data).youtube, text("https://www.youtube.com/watch?v=ccccccccccc"));

        let thumbnail = r#"<img src="https://i.ytimg.com/vi/ddddddddddd/hq.jpg">"#;
        assert_eq!(parse_page(thumbnail).youtube, text("https://www.youtube.com/watch?v=ddddddddddd"));
    }

    #[test]
    fn page_titles_come_in_two_forms() {
        assert_eq!(split_page_title("Song by Artist"), (text("Song"), text("Artist")));
        assert_eq!(split_page_title("Artist - Topic - Song"), (text("Topic - Song"), text("Artist")));
        assert_eq!(split_page_title("Song"), (text("Song"), None));
        let html = r#"<meta property="og:title" content="Rock &amp; Roll by AC&#x27;s"/>"#;
        assert_eq!(parse_page(html).title, text("Rock & Roll"));
        assert_eq!(parse_page(html).artist, text("AC's"));
    }

    #[test]
    fn what_to_download_depends_on_what_is_known() {
        let link = "https://song.link/s/x";
        let song = Song { youtube: text("https://youtu.be/a"), title: text("T"), artist: text("A") };
        assert_eq!(song.download_target(link), "https://youtu.be/a");
        let song = Song { youtube: None, title: text("T"), artist: text("A") };
        assert_eq!(song.download_target(link), "ytsearch1:A T");
        let song = Song { youtube: None, title: text("T"), artist: None };
        assert_eq!(song.download_target(link), "ytsearch1:T");
        assert_eq!(Song::default().download_target(link), link);
    }
}
