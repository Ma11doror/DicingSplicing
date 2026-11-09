//! Talking to Telegram: the message, the circle, and taking the message back if the circle didn't go.

use crate::credentials::scrub;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ureq::http::Response;
use ureq::{Body, SendBody};

const USER_AGENT: &str = concat!("dicing_splicing-telegram/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// A video has to be looked at on the other side before there is an answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);
/// How much the upload should move before saying so again.
const PROGRESS_STEP: f32 = 0.01;

#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn trigger(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Deserialize)]
struct Answer {
    ok: bool,
    #[serde(default)]
    description: String,
    #[serde(default)]
    result: Value,
}

/// The circle to send.
pub struct Note<'a> {
    pub chat: &'a str,
    pub length: u32,
    pub duration: u32,
}

/// How the circle went.
#[derive(Debug, PartialEq)]
pub struct Delivered {
    /// Telegram's id for the file, if it took it as a circle.
    pub file_id: Option<String>,
    /// Telegram may decide a file is not fit to be a circle and show it as an ordinary video.
    pub round: bool,
    pub uploaded: bool,
}

pub struct Api {
    agent: ureq::Agent,
    base: String,
    token: String,
}

impl Api {
    pub fn new(base: &str, token: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // The address holds the token: it goes nowhere else.
            .max_redirects(0)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_per_write(Some(WRITE_TIMEOUT))
            .timeout_recv_response(Some(ANSWER_TIMEOUT))
            .user_agent(USER_AGENT)
            .build()
            .into();
        Self { agent, base: base.to_owned(), token: token.to_owned() }
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base, self.token)
    }

    fn read_answer(&self, response: Result<Response<Body>, ureq::Error>) -> Result<Value> {
        let hidden = |text: String| anyhow!(scrub(&text, &self.token));
        let mut response = response.map_err(|error| hidden(format!("Telegram can't be reached: {error}")))?;
        let status = response.status();
        let text = response.body_mut().read_to_string().map_err(|error| hidden(format!("Telegram's answer can't be read: {error}")))?;
        let answer: Answer = serde_json::from_str(&text).map_err(|_| anyhow!("Telegram gave an unexpected answer (status {})", status.as_u16()))?;
        if !answer.ok {
            return Err(hidden(format!("Telegram says: {}", answer.description)));
        }
        Ok(answer.result)
    }

    fn post_form(&self, method: &str, fields: &[(&str, &str)]) -> Result<Value> {
        self.read_answer(self.agent.post(self.url(method)).send_form(fields.iter().copied()))
    }

    /// Returns the message's id.
    pub fn send_message(&self, chat: &str, text: &str) -> Result<i64> {
        let result = self.post_form("sendMessage", &[("chat_id", chat), ("text", text), ("parse_mode", "MarkdownV2"), ("disable_web_page_preview", "true")])?;
        result["message_id"].as_i64().context("Telegram didn't say which message it made")
    }

    pub fn delete_message(&self, chat: &str, message: i64) -> Result<()> {
        self.post_form("deleteMessage", &[("chat_id", chat), ("message_id", &message.to_string())]).map(|_| ())
    }

    /// Sends a circle that Telegram already has.
    pub fn resend_note(&self, note: &Note, file_id: &str) -> Result<Delivered> {
        let (length, duration) = (note.length.to_string(), note.duration.to_string());
        let result = self.post_form("sendVideoNote", &[("chat_id", note.chat), ("video_note", file_id), ("length", &length), ("duration", &duration)])?;
        Ok(delivered(&result, false))
    }

    /// Uploads a circle. `progress` hears how far the upload is, from 0 to 1.
    pub fn upload_note(&self, note: &Note, file: &Path, cancel: &Cancel, progress: &mut dyn FnMut(f32)) -> Result<Delivered> {
        let opened = File::open(file).with_context(|| format!("can't read {}", file.display()))?;
        let size = opened.metadata().with_context(|| format!("can't read {}", file.display()))?.len();
        let boundary = boundary();
        let (head, tail) = multipart_frame(&boundary, note);
        let total = head.len() as u64 + size + tail.len() as u64;
        let contents = Cursor::new(head).chain(opened).chain(Cursor::new(tail));
        let mut upload = Upload { contents, total, sent: 0, told: -1.0, cancel, progress };

        // With the length said in advance: not every server takes a body in chunks.
        let request = self.agent.post(self.url("sendVideoNote")).header("Content-Type", format!("multipart/form-data; boundary={boundary}")).header("Content-Length", total);
        let result = self.read_answer(request.send(SendBody::from_reader(&mut upload)))?;
        Ok(delivered(&result, true))
    }
}

fn delivered(result: &Value, uploaded: bool) -> Delivered {
    let file_id = result["video_note"]["file_id"].as_str().map(str::to_owned);
    Delivered { round: file_id.is_some(), file_id, uploaded }
}

/// Something that won't be met inside a video.
fn boundary() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos());
    format!("DicingSplicing{nanos:x}{:x}", std::process::id())
}

/// What goes before and after the file in the request.
fn multipart_frame(boundary: &str, note: &Note) -> (Vec<u8>, Vec<u8>) {
    let mut head = String::new();
    for (name, value) in [("chat_id", note.chat.to_owned()), ("length", note.length.to_string()), ("duration", note.duration.to_string())] {
        head.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"));
    }
    head.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"video_note\"; filename=\"circle.mp4\"\r\nContent-Type: video/mp4\r\n\r\n"));
    (head.into_bytes(), format!("\r\n--{boundary}--\r\n").into_bytes())
}

/// The request's body as it goes out: this is where its progress is seen and where it can be stopped.
struct Upload<'a, R> {
    contents: R,
    total: u64,
    sent: u64,
    told: f32,
    cancel: &'a Cancel,
    progress: &'a mut dyn FnMut(f32),
}

impl<R: Read> Read for Upload<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let read = self.contents.read(buffer)?;
        self.sent += read as u64;
        let fraction = self.sent as f32 / self.total.max(1) as f32;
        // The end is always told: with its size said in advance, the body is not read past its last byte.
        if fraction - self.told >= PROGRESS_STEP || (self.sent >= self.total && self.told < 1.0) {
            self.told = fraction;
            (self.progress)(fraction);
        }
        Ok(read)
    }
}

/// What is being done, for the user to see.
pub enum Step {
    Message,
    Resending,
    Uploading(f32),
}

/// Sending stopped because it was asked to, and left nothing behind.
#[derive(Debug)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("Cancelled")
    }
}

impl std::error::Error for Stopped {}

/// The circle: by its id if Telegram has it, otherwise uploaded. An id can stop being good (another
/// bot, say), and that is no reason to give up on sending.
fn send_note(api: &Api, note: &Note, file: &Path, known_id: Option<&str>, cancel: &Cancel, step: &mut dyn FnMut(Step)) -> Result<Delivered> {
    if let Some(file_id) = known_id {
        step(Step::Resending);
        if let Ok(delivered) = api.resend_note(note, file_id) {
            return Ok(delivered);
        }
    }
    if cancel.is_cancelled() {
        return Err(Stopped.into());
    }
    api.upload_note(note, file, cancel, &mut |fraction| step(Step::Uploading(fraction)))
}

/// Sends the message and then the circle, like the old script. If the circle doesn't go, the message
/// is taken back: a caption with no video under it should not stay in a channel.
pub fn deliver(api: &Api, note: &Note, caption: Option<&str>, file: &Path, known_id: Option<&str>, cancel: &Cancel, step: &mut dyn FnMut(Step)) -> Result<Delivered> {
    if cancel.is_cancelled() {
        return Err(Stopped.into());
    }
    let message = match caption {
        Some(text) => {
            step(Step::Message);
            Some(api.send_message(note.chat, text)?)
        }
        None => None,
    };
    let result = if cancel.is_cancelled() { Err(Stopped.into()) } else { send_note(api, note, file, known_id, cancel, step) };
    let failure = match result {
        Ok(delivered) => return Ok(delivered),
        // Whatever the connection said about being cut, this is why.
        Err(_) if cancel.is_cancelled() => anyhow!(Stopped),
        Err(failure) => failure,
    };
    match message.map(|message| api.delete_message(note.chat, message)) {
        Some(Err(not_deleted)) => bail!("{failure:#}. The message stayed in the channel, delete it by hand ({not_deleted:#})"),
        _ => Err(failure),
    }
}

#[cfg(test)]
pub mod fake {
    //! A pretend Telegram on this computer. No test here ever talks to the real one.

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[derive(Debug, Clone)]
    pub struct Request {
        pub method: String,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Request {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
        }

        pub fn text(&self) -> String {
            String::from_utf8_lossy(&self.body).into_owned()
        }
    }

    pub struct FakeTelegram {
        pub url: String,
        requests: Arc<Mutex<Vec<Request>>>,
    }

    impl FakeTelegram {
        /// Answers each request with the next of `replies`.
        pub fn start(replies: &[&str]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let (seen, replies): (_, Vec<String>) = (requests.clone(), replies.iter().map(|reply| reply.to_string()).collect());
            thread::spawn(move || {
                let mut replies = replies.into_iter();
                for stream in listener.incoming().map_while(Result::ok) {
                    let reply = replies.next().unwrap_or_else(|| r#"{"ok":false,"description":"no more replies"}"#.to_owned());
                    let _ = serve(stream, &reply, &seen);
                }
            });
            Self { url, requests }
        }

        pub fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }

        /// The Bot API methods called, in order.
        pub fn methods(&self) -> Vec<String> {
            self.requests().into_iter().map(|request| request.method).collect()
        }
    }

    fn serve(stream: TcpStream, reply: &str, seen: &Mutex<Vec<Request>>) -> Option<()> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let path = line.split_whitespace().nth(1)?.to_owned();
        let mut headers = Vec::new();
        loop {
            line.clear();
            reader.read_line(&mut line).ok()?;
            let Some((key, value)) = line.trim_end().split_once(':') else { break };
            headers.push((key.trim().to_owned(), value.trim().to_owned()));
        }
        let length: usize = headers.iter().find(|(key, _)| key.eq_ignore_ascii_case("content-length")).and_then(|(_, value)| value.parse().ok()).unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
        // Noted before answering: whoever gets the answer may look at once.
        seen.lock().unwrap().push(Request { method: path.rsplit('/').next()?.to_owned(), headers, body });
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
        reader.get_mut().write_all(response.as_bytes()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeTelegram;
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    const TOKEN: &str = "123456:fake-Token_value";
    const MESSAGE_OK: &str = r#"{"ok":true,"result":{"message_id":77}}"#;
    const NOTE_OK: &str = r#"{"ok":true,"result":{"message_id":78,"video_note":{"file_id":"FILE1","length":400}}}"#;
    const DELETED: &str = r#"{"ok":true,"result":true}"#;
    const REFUSED: &str = r#"{"ok":false,"error_code":400,"description":"Bad Request: nope"}"#;
    const NOTE: Note = Note { chat: "@test", length: 400, duration: 12 };
    /// Bytes that an upload that treats them as text would spoil.
    const VIDEO: &[u8] = b"video\r\n--\0\xFF\xFE bytes";

    fn video_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_telegram_api_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mp4");
        fs::write(&file, VIDEO).unwrap();
        file
    }

    fn run(server: &FakeTelegram, caption: Option<&str>, file: &Path, known_id: Option<&str>, cancel: &Cancel) -> Result<Delivered> {
        deliver(&Api::new(&server.url, TOKEN), &NOTE, caption, file, known_id, cancel, &mut |_| {})
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|window| window == needle)
    }

    #[test]
    fn the_message_goes_first_and_then_the_circle() {
        let file = video_file("order");
        let server = FakeTelegram::start(&[MESSAGE_OK, NOTE_OK]);
        let delivered = run(&server, Some("[a](https://b)"), &file, None, &Cancel::default()).unwrap();
        assert_eq!(delivered, Delivered { file_id: Some("FILE1".into()), round: true, uploaded: true });
        assert_eq!(server.methods(), ["sendMessage", "sendVideoNote"]);

        let requests = server.requests();
        let message = requests[0].text();
        assert!(message.contains("chat_id=%40test"), "{message}");
        assert!(message.contains("parse_mode=MarkdownV2"), "{message}");
        assert!(message.contains("disable_web_page_preview=true"), "{message}");
        assert!(message.contains("text=%5Ba%5D(https%3A%2F%2Fb)"), "{message}");

        let upload = &requests[1];
        assert!(upload.header("content-type").unwrap().starts_with("multipart/form-data; boundary="));
        assert_eq!(upload.header("content-length").unwrap().parse::<usize>().unwrap(), upload.body.len());
        assert_eq!(upload.header("transfer-encoding"), None, "the size is said in advance");
        assert!(contains(&upload.body, VIDEO), "the file goes out byte for byte");
        let form = String::from_utf8_lossy(&upload.body).into_owned();
        for field in ["name=\"chat_id\"\r\n\r\n@test\r\n", "name=\"length\"\r\n\r\n400\r\n", "name=\"duration\"\r\n\r\n12\r\n", "name=\"video_note\"; filename=\"circle.mp4\""] {
            assert!(form.contains(field), "{field} is not in {form}");
        }
    }

    #[test]
    fn with_nothing_to_say_only_the_circle_goes() {
        let file = video_file("no_caption");
        let server = FakeTelegram::start(&[NOTE_OK]);
        run(&server, None, &file, None, &Cancel::default()).unwrap();
        assert_eq!(server.methods(), ["sendVideoNote"]);
    }

    #[test]
    fn if_the_message_is_refused_the_circle_stays_home() {
        let file = video_file("message_refused");
        let server = FakeTelegram::start(&[REFUSED]);
        let error = run(&server, Some("x"), &file, None, &Cancel::default()).unwrap_err();
        assert!(format!("{error:#}").contains("nope"), "{error:#}");
        assert_eq!(server.methods(), ["sendMessage"]);
    }

    #[test]
    fn if_the_circle_doesnt_go_the_message_is_taken_back() {
        let file = video_file("take_back");
        let server = FakeTelegram::start(&[MESSAGE_OK, REFUSED, DELETED]);
        let error = run(&server, Some("x"), &file, None, &Cancel::default()).unwrap_err();
        assert!(format!("{error:#}").contains("nope"), "{error:#}");
        assert_eq!(server.methods(), ["sendMessage", "sendVideoNote", "deleteMessage"]);
        let deleting = server.requests()[2].text();
        assert!(deleting.contains("message_id=77") && deleting.contains("chat_id=%40test"), "{deleting}");
    }

    #[test]
    fn a_message_that_cant_be_taken_back_is_mentioned() {
        let file = video_file("not_taken_back");
        let server = FakeTelegram::start(&[MESSAGE_OK, REFUSED, r#"{"ok":false,"description":"message can't be deleted"}"#]);
        let error = format!("{:#}", run(&server, Some("x"), &file, None, &Cancel::default()).unwrap_err());
        assert!(error.contains("nope") && error.contains("delete it by hand") && error.contains("can't be deleted"), "{error}");
    }

    #[test]
    fn a_known_circle_goes_without_an_upload() {
        let file = video_file("resend");
        let server = FakeTelegram::start(&[MESSAGE_OK, NOTE_OK]);
        let delivered = run(&server, Some("x"), &file, Some("KNOWN"), &Cancel::default()).unwrap();
        assert!(!delivered.uploaded);
        let resend = &server.requests()[1];
        assert_eq!(resend.method, "sendVideoNote");
        assert!(resend.text().contains("video_note=KNOWN"), "{}", resend.text());
        assert!(!contains(&resend.body, VIDEO));
    }

    #[test]
    fn an_id_that_is_no_good_means_uploading_after_all() {
        let file = video_file("bad_id");
        let server = FakeTelegram::start(&[r#"{"ok":false,"description":"Bad Request: wrong file identifier"}"#, NOTE_OK]);
        let delivered = run(&server, None, &file, Some("STALE"), &Cancel::default()).unwrap();
        assert!(delivered.uploaded);
        assert_eq!(server.methods(), ["sendVideoNote", "sendVideoNote"]);
        assert!(contains(&server.requests()[1].body, VIDEO));
    }

    #[test]
    fn telegram_may_not_take_it_as_a_circle() {
        let file = video_file("not_round");
        let server = FakeTelegram::start(&[r#"{"ok":true,"result":{"message_id":1,"video":{"file_id":"V"}}}"#]);
        let delivered = run(&server, None, &file, None, &Cancel::default()).unwrap();
        assert_eq!(delivered, Delivered { file_id: None, round: false, uploaded: true });
    }

    #[test]
    fn cancelling_before_anything_sends_nothing() {
        let file = video_file("cancel_first");
        let server = FakeTelegram::start(&[]);
        let cancel = Cancel::default();
        cancel.trigger();
        assert!(run(&server, Some("x"), &file, None, &cancel).unwrap_err().is::<Stopped>());
        assert!(server.methods().is_empty());
    }

    #[test]
    fn stopping_that_leaves_a_message_behind_is_not_quiet() {
        let file = video_file("cancel_left_behind");
        let server = FakeTelegram::start(&[MESSAGE_OK, r#"{"ok":false,"description":"message can't be deleted"}"#]);
        let cancel = Cancel::default();
        let on_step = cancel.clone();
        let api = Api::new(&server.url, TOKEN);
        let mut stop_when_message_is_sent = |step: Step| {
            if matches!(step, Step::Message) {
                on_step.trigger();
            }
        };
        let error = deliver(&api, &NOTE, Some("x"), &file, None, &cancel, &mut stop_when_message_is_sent).unwrap_err();
        assert!(!error.is::<Stopped>(), "the user has to hear about this");
        assert!(format!("{error:#}").contains("delete it by hand"), "{error:#}");
    }

    #[test]
    fn cancelling_during_the_upload_takes_the_message_back() {
        let file = video_file("cancel_upload");
        // The upload never reaches the server, so the delete is its next request.
        let server = FakeTelegram::start(&[MESSAGE_OK, DELETED]);
        let cancel = Cancel::default();
        let on_step = cancel.clone();
        let api = Api::new(&server.url, TOKEN);
        let mut stop_when_message_is_sent = |step: Step| {
            if matches!(step, Step::Message) {
                on_step.trigger();
            }
        };
        assert!(deliver(&api, &NOTE, Some("x"), &file, None, &cancel, &mut stop_when_message_is_sent).unwrap_err().is::<Stopped>());
        let methods = server.methods();
        assert_eq!(methods.first().map(String::as_str), Some("sendMessage"));
        assert_eq!(methods.last().map(String::as_str), Some("deleteMessage"), "{methods:?}");
        assert!(!methods.iter().any(|method| method == "sendVideoNote"), "{methods:?}");
    }

    #[test]
    fn an_upload_says_how_far_it_is() {
        let file = video_file("progress");
        fs::write(&file, vec![7u8; 300_000]).unwrap();
        let server = FakeTelegram::start(&[NOTE_OK]);
        let mut seen = Vec::new();
        Api::new(&server.url, TOKEN).upload_note(&NOTE, &file, &Cancel::default(), &mut |fraction| seen.push(fraction)).unwrap();
        assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "{seen:?}");
        assert_eq!(seen.last().copied(), Some(1.0), "{seen:?}");
        assert!(seen.len() <= 102, "{}", seen.len());
    }

    #[test]
    fn the_token_never_shows_in_an_error() {
        let file = video_file("token");
        // Telegram itself repeats the token.
        let echo = format!(r#"{{"ok":false,"description":"bad bot{TOKEN}"}}"#);
        let server = FakeTelegram::start(&[&echo]);
        let error = format!("{:#}", run(&server, None, &file, None, &Cancel::default()).unwrap_err());
        assert!(error.contains("<token>") && !error.contains(TOKEN), "{error}");

        // Nobody listens there: the client's own message is the one to watch.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let nobody = Api::new(&format!("http://127.0.0.1:{port}"), TOKEN);
        let error = format!("{:#}", deliver(&nobody, &NOTE, Some("x"), &file, None, &Cancel::default(), &mut |_| {}).unwrap_err());
        assert!(!error.contains(TOKEN) && !error.contains("fake-Token"), "{error}");

        // Something that is not Telegram at all.
        let server = FakeTelegram::start(&[&format!("<html>{TOKEN}</html>")]);
        let error = format!("{:#}", run(&server, None, &file, None, &Cancel::default()).unwrap_err());
        assert!(!error.contains(TOKEN), "{error}");
    }

    #[test]
    fn a_redirect_isnt_followed() {
        // A server that sends us elsewhere would get the token in the address.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let elsewhere = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let elsewhere_url = format!("http://{}/stolen", elsewhere.local_addr().unwrap());
        elsewhere.set_nonblocking(true).unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.read(&mut [0; 4096]);
            let reply = format!("HTTP/1.1 302 Found\r\nLocation: {elsewhere_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(reply.as_bytes());
        });
        assert!(Api::new(&url, TOKEN).send_message("@test", "x").is_err());
        assert!(elsewhere.accept().is_err(), "nothing should have come here");
    }
}
