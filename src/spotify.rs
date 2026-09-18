//! Spotify search, for turning what you typed into a real record.
//!
//! Two problems this solves, and the second is the one that matters.
//!
//! The obvious one is spelling: you type half a title and get the catalogue's
//! version of it back.
//!
//! The one underneath is that the downloader's resolver is good at refusing
//! the wrong *kind* of upload -- live takes, music videos, karaoke, pitched
//! re-uploads -- and much weaker at telling the right song from a featurette
//! about the song. Asked for "Weezer - Buddy Holly" it scores the record and
//! "The Making of 'Buddy Holly'" identically, because on title alone they
//! look the same. It already knows how to use an expected duration and has
//! never been given one. Spotify has the duration. That is the fix.
//!
//! Client-credentials only: this reads the public catalogue and never touches
//! an account, so there is no user login and no refresh token to keep.

use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// One result, flattened to what a request actually needs.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration_ms: u64,
    pub year: Option<String>,
}

impl Suggestion {
    pub fn payload(&self) -> serde_json::Value {
        serde_json::json!({"artist": self.artist, "title": self.title, "album": self.album,
                          "year": self.year, "duration_ms": self.duration_ms})
    }
    /// What goes into the request box, and what the resolver searches for.
    pub fn query(&self) -> String {
        format!("{} - {}", self.artist, self.title)
    }

    pub fn length(&self) -> String {
        let whole = self.duration_ms / 1000;
        format!("{}:{:02}", whole / 60, whole % 60)
    }
}

/// Credentials, read once out of the project's `.env`.
#[derive(Clone)]
pub struct Credentials {
    id: String,
    secret: String,
}

pub fn credentials(root: &Path) -> Option<Credentials> {
    let text = std::fs::read_to_string(root.join(".env")).ok()?;
    let mut id = None;
    let mut secret = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches(['"', '\'']).to_string();
        if value.is_empty() {
            continue;
        }
        match name.trim() {
            "SPOTIFY_CLIENT_ID" => id = Some(value),
            "SPOTIFY_CLIENT_SECRET" => secret = Some(value),
            _ => {}
        }
    }
    Some(Credentials { id: id?, secret: secret? })
}

/// A token, and when it stops being one.
struct Token {
    value: String,
    expires: Instant,
}

/// Searches run on their own thread; the console never waits on the network.
pub struct Search {
    credentials: Option<Credentials>,
    results: Receiver<(String, Result<Vec<Suggestion>, String>)>,
    sender: Sender<(String, Result<Vec<Suggestion>, String>)>,

    /// What the user has typed, and when they last touched it.
    pending: Option<(String, Instant)>,
    /// The query currently in flight, so a slower earlier reply is ignored.
    inflight: Option<String>,
    /// What is on screen, and which query produced it.
    pub showing: Vec<Suggestion>,
    pub showing_for: String,
    pub error: Option<String>,
    pub busy: bool,
}

/// Long enough that typing a title does not fire a request per keystroke,
/// short enough that stopping feels like an answer arriving.
const SETTLE: Duration = Duration::from_millis(280);

impl Search {
    pub fn new(root: &Path) -> Self {
        let (sender, results) = channel();
        Search {
            credentials: credentials(root),
            results,
            sender,
            pending: None,
            inflight: None,
            showing: Vec::new(),
            showing_for: String::new(),
            error: None,
            busy: false,
        }
    }

    pub fn available(&self) -> bool {
        self.credentials.is_some()
    }

    /// Called every frame with whatever is in the box.
    pub fn typed(&mut self, query: &str) {
        let query = query.trim().to_string();
        if query == self.showing_for && self.pending.is_none() {
            return;
        }
        match &self.pending {
            Some((last, _)) if *last == query => {}
            _ => {
                self.inflight = None;
                self.showing.clear();
                self.error = None;
                self.busy = false;
                self.pending = Some((query, Instant::now()));
            }
        }
    }

    pub fn clear(&mut self) {
        self.pending = None;
        self.inflight = None;
        self.showing.clear();
        self.showing_for.clear();
        self.error = None;
        self.busy = false;
    }

    /// Fire anything that has settled, and take anything that has come back.
    pub fn tick(&mut self) {
        while let Ok((query, outcome)) = self.results.try_recv() {
            // A reply for a query the user has already typed past is stale.
            if self.inflight.as_deref() != Some(query.as_str()) {
                continue;
            }
            self.inflight = None;
            self.busy = false;
            match outcome {
                Ok(found) => {
                    self.showing = found;
                    self.showing_for = query;
                    self.error = None;
                }
                Err(error) => {
                    self.showing.clear();
                    self.error = Some(error);
                }
            }
        }

        let Some((query, since)) = self.pending.clone() else { return };
        if since.elapsed() < SETTLE {
            return;
        }
        self.pending = None;

        // Two characters is where a search stops being every record ever.
        let lower = query.to_lowercase();
        if query.chars().count() < 2 || lower.contains("http://") || lower.contains("https://")
            || lower.contains("youtube.com/") || lower.contains("youtu.be/") {
            self.showing.clear();
                self.showing_for = query;
            return;
        }
        let Some(credentials) = self.credentials.clone() else { return };

        self.inflight = Some(query.clone());
        self.busy = true;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let outcome = search(&credentials, &query);
            let _ = sender.send((query, outcome));
        });
    }
}

/// The token, cached for as long as Spotify says it is good for.
///
/// A mutex rather than a channel: this is contended once every hour, and only
/// ever from a search thread.
static TOKEN: std::sync::Mutex<Option<Token>> = std::sync::Mutex::new(None);

fn token(credentials: &Credentials) -> Result<String, String> {
    if let Ok(held) = TOKEN.lock() {
        if let Some(token) = held.as_ref() {
            if token.expires > Instant::now() {
                return Ok(token.value.clone());
            }
        }
    }

    let basic = base64(format!("{}:{}", credentials.id, credentials.secret).as_bytes());
    let response = ureq::post("https://accounts.spotify.com/api/token")
        .header("Authorization", &format!("Basic {basic}"))
        .send_form([("grant_type", "client_credentials")])
        .map_err(|error| friendly(error, "could not reach Spotify"))?
        .body_mut()
        .read_json::<serde_json::Value>()
        .map_err(|_| "Spotify sent something unreadable".to_string())?;

    let value = response["access_token"]
        .as_str()
        .ok_or("Spotify refused the credentials")?
        .to_string();
    // A minute short, so a token never expires between being checked and used.
    let seconds = response["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60);

    if let Ok(mut held) = TOKEN.lock() {
        *held = Some(Token {
            value: value.clone(),
            expires: Instant::now() + Duration::from_secs(seconds),
        });
    }
    Ok(value)
}

fn search(credentials: &Credentials, query: &str) -> Result<Vec<Suggestion>, String> {
    let token = token(credentials)?;

    let body = ureq::get("https://api.spotify.com/v1/search")
        .header("Authorization", &format!("Bearer {token}"))
        .query("q", query)
        .query("type", "track")
        .query("limit", "8")
        .call()
        .map_err(|error| friendly(error, "the search failed"))?
        .body_mut()
        .read_json::<serde_json::Value>()
        .map_err(|_| "Spotify sent something unreadable".to_string())?;

    let items = body["tracks"]["items"]
        .as_array()
        .ok_or("no results in the reply")?;

    Ok(items.iter().filter_map(suggestion).collect())
}

fn suggestion(item: &serde_json::Value) -> Option<Suggestion> {
    let title = item["name"].as_str()?.to_string();

    // Every credited artist, because "x feat. y" is how half a crate is named
    // and dropping the feature loses the search.
    let artist = item["artists"]
        .as_array()?
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if artist.is_empty() {
        return None;
    }

    let album = item["album"]["name"].as_str().map(str::to_string);
    let year = item["album"]["release_date"]
        .as_str()
        .and_then(|d| d.get(..4))
        .map(str::to_string);

    Some(Suggestion {
        artist,
        title,
        album,
        duration_ms: item["duration_ms"].as_u64().unwrap_or(0),
        year,
    })
}

/// Network errors, said in a way that suggests what to do about them.
fn friendly(error: ureq::Error, context: &str) -> String {
    match error {
        ureq::Error::StatusCode(401) => "Spotify refused the credentials".into(),
        ureq::Error::StatusCode(429) => "Spotify is rate limiting; wait a moment".into(),
        ureq::Error::StatusCode(code) => format!("{context} ({code})"),
        other => format!("{context}: {other}"),
    }
}

/// Standard base64. One small function beats a dependency for the single
/// place this is needed.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        // The RFC 4648 vectors, including both padding lengths.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_suggestion_joins_every_credited_artist() {
        let item: serde_json::Value = serde_json::from_str(
            r#"{"name":"SPAGHETTI",
                "artists":[{"name":"LE SSERAFIM"},{"name":"j-hope"}],
                "album":{"name":"EASY","release_date":"2024-02-19"},
                "duration_ms":172000}"#,
        )
        .unwrap();
        let found = suggestion(&item).unwrap();
        assert_eq!(found.artist, "LE SSERAFIM, j-hope");
        assert_eq!(found.query(), "LE SSERAFIM, j-hope - SPAGHETTI");
        assert_eq!(found.length(), "2:52");
        assert_eq!(found.year.as_deref(), Some("2024"));
    }

    #[test]
    fn a_track_with_no_artist_is_not_a_suggestion() {
        let item: serde_json::Value =
            serde_json::from_str(r#"{"name":"x","artists":[],"duration_ms":1}"#).unwrap();
        assert!(suggestion(&item).is_none());
    }

    #[test]
    fn missing_credentials_are_absence_rather_than_failure() {
        let nowhere = std::env::temp_dir().join("defalt-no-such-project");
        assert!(credentials(&nowhere).is_none());
    }

    /// Against the real API, so credentials, the token exchange and the
    /// parsing are all proved together. Ignored by default: it needs the
    /// network and a key.
    #[test]
    #[ignore = "needs Spotify credentials and the network"]
    fn a_real_search_comes_back_with_records() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let credentials = credentials(&root).expect("no SPOTIFY_ credentials in .env");

        let found = search(&credentials, "Weezer Buddy Holly").expect("search failed");
        assert!(!found.is_empty(), "no results");

        for one in found.iter().take(4) {
            println!(
                "  {:<34} {:<24} {} {}",
                one.title,
                one.artist,
                one.length(),
                one.year.clone().unwrap_or_default()
            );
        }

        let record = found
            .iter()
            .find(|s| s.title.to_lowercase().starts_with("buddy holly"))
            .expect("the record itself was not in the results");
        // The whole reason this exists: a duration the resolver can score on.
        assert!(record.duration_ms > 60_000, "implausible duration");
        assert!(record.artist.to_lowercase().contains("weezer"));
    }

    #[test]
    fn a_single_character_never_reaches_the_network() {
        let mut search = Search::new(&std::env::temp_dir());
        search.typed("a");
        // Pretend it settled.
        search.pending = Some(("a".into(), Instant::now() - SETTLE * 2));
        search.tick();
        assert!(search.inflight.is_none());
        assert!(!search.busy);
    }

    #[test]
    fn links_do_not_search_and_typing_invalidates_old_results_immediately() {
        let mut search = Search::new(&std::env::temp_dir());
        search.inflight = Some("old".into());
        search.typed("new");
        search.sender.send(("old".into(), Ok(vec![Suggestion {
            artist: "Artist".into(), title: "Old result".into(), album: None,
            duration_ms: 123000, year: None,
        }]))).unwrap();
        search.tick();
        assert!(search.showing.is_empty());
        search.pending = Some(("https://youtu.be/dQw4w9WgXcQ".into(), Instant::now() - SETTLE * 2));
        search.tick();
        assert!(search.inflight.is_none());
        assert!(!search.busy);
    }

    #[test]
    fn radio_selection_keeps_duration_and_album() {
        let found = Suggestion { artist: "Artist".into(), title: "Song".into(), album: Some("Album".into()),
            duration_ms: 123456, year: Some("2024".into()) };
        let payload = found.payload();
        assert_eq!(payload["duration_ms"], 123456);
        assert_eq!(payload["album"], "Album");
        assert_eq!(payload["year"], "2024");
    }
}
