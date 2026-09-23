//! Bug reports and suggestions, written where you can work through them later.
//!
//! The station files reports itself (radio/feedback.py) and the console
//! sends it one when it is up. When it is not -- which is exactly when a
//! report is most likely -- the console writes the same thing itself:
//! reports/INBOX.md gets a line, and reports/<id>/ gets report.md,
//! context.json, logs.txt and screenshot.png. The two writers share one
//! format, described in docs/FEEDBACK.md; change one, change both.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::logfile;

pub const WINDOW_BEFORE_MS: i64 = 10 * 60 * 1000;
pub const WINDOW_AFTER_MS: i64 = 60 * 1000;
pub const MAX_LOG_LINES: usize = 2000;
const MAX_TITLE: usize = 200;

/// Kept identical to radio/feedback.py's, byte for byte.
pub const INBOX_HEADER: &str = "# Feedback inbox

Bug reports and suggestions filed from the console and the radio page, oldest
first. Each line points at a folder holding report.md, context.json and
logs.txt (and screenshot.png when one was taken). Close an item with
`python -m radio.feedback close <id> \"what was done\"`.

";

#[derive(Clone, Debug, Default)]
pub struct Report {
    /// "bug" or "suggestion".
    pub kind: String,
    pub title: String,
    pub description: String,
    pub expected: String,
    pub attach_logs: bool,
    /// What the console knew, under "console" in context.json.
    pub context: Value,
    pub screenshot_png: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filed {
    pub id: String,
    pub path: String,
    /// True when the station took it, false when the console wrote it.
    pub by_station: bool,
}

/* ── Filing ──────────────────────────────────────────────────────────── */

/// Hand the report to the station, or write it here if the station cannot
/// take it. A report the station refused as malformed is not retried here:
/// the same report would be just as malformed on disk.
pub fn file(root: &Path, station: Option<&str>, report: &Report) -> Result<Filed, String> {
    let at = logfile::now_ms();
    if let Some(url) = station {
        match post(url, report, &console_excerpt(&root.join("logs"), at)) {
            Ok(filed) => return Ok(filed),
            Err(Refusal::Rejected(error)) => return Err(error),
            Err(Refusal::Unreachable(why)) => {
                logfile::log!("feedback: station could not take the report ({why}); writing it here");
            }
        }
    }
    write_offline(root, report, at, logfile::offset(), &Redactor::from_env(root))
}

enum Refusal {
    Rejected(String),
    Unreachable(String),
}

fn post(url: &str, report: &Report, logs: &[String]) -> Result<Filed, Refusal> {
    let mut body = json!({
        "kind": report.kind, "title": report.title, "description": report.description,
        "expected": report.expected, "attach_logs": report.attach_logs, "client": "console",
        "client_context": report.context,
        "client_logs": if report.attach_logs { logs.to_vec() } else { Vec::new() },
    });
    if let Some(png) = &report.screenshot_png {
        body["screenshot_png"] = base64(png).into();
    }
    let mut response = ureq::post(&format!("{url}/api/feedback"))
        .config().http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(20))).build()
        .send_json(body)
        .map_err(|error| Refusal::Unreachable(error.to_string()))?;
    let status = response.status().as_u16();
    let value: Value = response.body_mut().read_json().unwrap_or(Value::Null);
    match status {
        200..=299 => match (value["id"].as_str(), value["path"].as_str()) {
            (Some(id), Some(path)) => Ok(Filed { id: id.into(), path: path.into(), by_station: true }),
            _ => Err(Refusal::Unreachable("an unreadable answer".into())),
        },
        400 => Err(Refusal::Rejected(
            value["error"].as_str().unwrap_or("The station refused the report.").to_string())),
        code => Err(Refusal::Unreachable(format!("it answered {code}"))),
    }
}

/// Write the whole report here, in the station's format.
pub fn write_offline(root: &Path, report: &Report, at_ms: i64, offset: i64,
                     scrub: &Redactor) -> Result<Filed, String> {
    let reports = root.join("reports");
    let kind = if report.kind == "suggestion" { "suggestion" } else { "bug" };
    let mut title = clean_line(&scrub.text(&report.title), MAX_TITLE);
    let description = scrub.text(report.description.trim());
    let expected = scrub.text(report.expected.trim());
    if title.is_empty() {
        title = clean_line(description.lines().next().unwrap_or(""), 80);
    }
    if title.is_empty() {
        return Err("Give the report a title or a description.".into());
    }

    let when = logfile::civil_at(at_ms, offset);
    let base = format!("{:04}{:02}{:02}-{:02}{:02}{:02}-{}",
        when.year, when.month, when.day, when.hour, when.minute, when.second, slug(&title));
    let folder = new_folder(&reports, &base).map_err(|e| format!("Could not write the report: {e}"))?;
    let id = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let filed_iso = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}",
        when.year, when.month, when.day, when.hour, when.minute, when.second, logfile::zone(offset));
    let filed_local = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        when.year, when.month, when.day, when.hour, when.minute, when.second);

    let environment = environment(root);
    let context = scrub.value(&json!({
        "id": id, "filed": filed_iso, "kind": kind, "client": "console",
        "environment": environment,
        "station": {"unavailable": {"station": "the station was not running; the console wrote this report"}},
        "console": report.context,
    }));
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(folder.join(name), bytes).map_err(|e| format!("Could not write {name}: {e}"))
    };
    write("context.json", serde_json::to_string_pretty(&context).unwrap_or_default().as_bytes())?;
    let mut attached = vec!["context.json".to_string()];

    if report.attach_logs {
        let lines = log_window(&root.join("logs"), at_ms, offset);
        let text: Vec<String> = lines.iter().map(|line| scrub.text(line)).collect();
        let body = if text.is_empty() { "(no log lines in the window)\n".to_string() } else { text.join("\n") + "\n" };
        write("logs.txt", body.as_bytes())?;
        attached.push(format!("logs.txt ({} lines, {} min before to {} min after)",
            lines.len(), WINDOW_BEFORE_MS / 60_000, WINDOW_AFTER_MS / 60_000));
    }
    if let Some(png) = &report.screenshot_png {
        write("screenshot.png", png)?;
        attached.push("screenshot.png".into());
    }

    let state = report.context["station"]["health"].as_str().unwrap_or("not running").to_string();
    let heading = if kind == "bug" { "Bug" } else { "Suggestion" };
    let mut md = vec![
        format!("# {heading}: {title}"), String::new(),
        format!("- id: {id}"),
        format!("- filed: {filed_local} ({filed_iso})"),
        format!("- type: {kind}"),
        "- status: open".into(),
        "- from: console".into(),
        format!("- app: {}", environment["app"].as_str().unwrap_or("")),
        format!("- commit: {} on {}", environment["commit"].as_str().unwrap_or("unknown"),
            environment["branch"].as_str().unwrap_or("unknown")),
        format!("- os: {}", environment["os"].as_str().unwrap_or("")),
        format!("- python: {}", environment["python"].as_str().unwrap_or("")),
        format!("- station: {state}"),
        String::new(),
        if kind == "bug" { "## What happened".into() } else { "## Suggestion".into() },
        String::new(),
        if description.is_empty() { "(no description)".into() } else { description },
        String::new(),
    ];
    if !expected.is_empty() {
        md.extend(["## What I expected".into(), String::new(), expected, String::new()]);
    }
    md.extend(["## Attached".into(), String::new()]);
    md.extend(attached.iter().map(|name| format!("- {name}")));
    md.push(String::new());
    write("report.md", md.join("\n").as_bytes())?;

    let line = format!("- [ ] {id} | {filed_local} | {kind} | open | {title} | {id}/report.md\n");
    append_inbox(&reports, &line).map_err(|e| format!("Could not update INBOX.md: {e}"))?;
    Ok(Filed { path: format!("reports/{id}/"), id, by_station: false })
}

fn new_folder(reports: &Path, base: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(reports)?;
    for attempt in 1..100 {
        let name = if attempt == 1 { base.to_string() } else { format!("{base}-{attempt}") };
        match std::fs::create_dir(reports.join(&name)) {
            Ok(()) => return Ok(reports.join(name)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other("no free report folder"))
}

fn append_inbox(reports: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let inbox = reports.join("INBOX.md");
    let fresh = !inbox.exists();
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(inbox)?;
    if fresh {
        file.write_all(INBOX_HEADER.as_bytes())?;
    }
    file.write_all(line.as_bytes())
}

fn environment(root: &Path) -> Value {
    let git = |args: &[&str]| -> String {
        crate::process::background("git").args(args).current_dir(root)
            .stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .output().ok().filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let commit = git(&["rev-parse", "--short", "HEAD"]);
    let dirty = !commit.is_empty() && !git(&["status", "--porcelain", "--untracked-files=no"]).is_empty();
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    json!({
        "app": format!("Defalt v{}", env!("CARGO_PKG_VERSION")),
        "version": env!("CARGO_PKG_VERSION"),
        "commit": if commit.is_empty() { "unknown".into() }
                  else if dirty { format!("{commit} (uncommitted changes)") } else { commit },
        "branch": if branch.is_empty() { "unknown".into() } else { branch },
        "os": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "python": "not involved (the console wrote this)",
    })
}

pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(40).collect();
    let out = out.trim_end_matches('-');
    if out.is_empty() { "report".into() } else { out.into() }
}

fn clean_line(text: &str, limit: usize) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "/").chars().take(limit).collect()
}

/* ── Logs ────────────────────────────────────────────────────────────── */

struct Entry {
    at: i64,
    order: usize,
    source: String,
    text: String,
}

fn log_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.file_name().and_then(|n| n.to_str()).is_some_and(|name| {
            let (_, tail) = match name.split_once(".log") { Some(parts) => parts, None => return false };
            tail.is_empty() || tail.strip_prefix('.').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }))
        .collect();
    files.sort();
    files
}

/// Every line of one file, with a line lacking a stamp of its own -- the
/// middle of a traceback -- given the one before it.
fn read_entries(path: &Path, source: &str, order: &mut usize, from: i64, to: i64, into: &mut Vec<Entry>) {
    let Ok(bytes) = std::fs::read(path) else { return };
    let text = String::from_utf8_lossy(&bytes);
    let mut last: Option<i64> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let stamped = line.split_once(' ').and_then(|(at, rest)| Some((logfile::parse_stamp(at)?, rest)));
        let (at, rest) = match (stamped, last) {
            (Some((at, rest)), _) => { last = Some(at); (at, rest) }
            (None, Some(at)) => (at, line),
            (None, None) => continue,
        };
        if (from..=to).contains(&at) {
            into.push(Entry { at, order: *order, source: source.to_string(), text: rest.to_string() });
            *order += 1;
        }
    }
}

/// Every log line near `at_ms`, from every log, oldest first -- the same
/// merge radio/feedback.py does. Where the console copied a line the
/// station also logged itself, only the station's copy is kept.
pub fn log_window(dir: &Path, at_ms: i64, offset: i64) -> Vec<String> {
    let (from, to) = (at_ms - WINDOW_BEFORE_MS, at_ms + WINDOW_AFTER_MS);
    let mut entries = Vec::new();
    let mut order = 0;
    for path in log_files(dir) {
        let recent = std::fs::metadata(&path).and_then(|m| m.modified()).ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_none_or(|t| t.as_millis() as i64 >= from);
        if !recent {
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let source = name.split(".log").next().unwrap_or("log").to_string();
        read_entries(&path, &source, &mut order, from, to, &mut entries);
    }
    entries.sort_by_key(|e| (e.at, e.order));

    let own: Vec<(i64, &str)> = entries.iter()
        .filter(|e| e.source == "station")
        .filter_map(|e| Some((e.at, e.text.strip_prefix("[out] ").or_else(|| e.text.strip_prefix("[err] "))?.trim())))
        .collect();
    let mut kept: Vec<String> = Vec::new();
    for entry in &entries {
        if entry.source != "station" {
            let echo = entry.text.strip_prefix("[station] ").or_else(|| entry.text.strip_prefix("[station:err] "));
            if let Some(echo) = echo {
                if own.iter().any(|(at, text)| *text == echo.trim() && (at - entry.at).abs() <= 10_000) {
                    continue;
                }
            }
        }
        kept.push(format!("{} {} {}", logfile::stamp(entry.at, offset), entry.source, entry.text));
    }
    if kept.len() > MAX_LOG_LINES {
        let dropped = kept.len() - MAX_LOG_LINES;
        kept.drain(..dropped);
        kept.insert(0, format!("... {dropped} earlier lines left out ..."));
    }
    kept
}

/// The console's own log lines near `at_ms`, raw, for the station to merge.
pub fn console_excerpt(dir: &Path, at_ms: i64) -> Vec<String> {
    let mut entries = Vec::new();
    let mut order = 0;
    for path in log_files(dir) {
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("console.log")) {
            read_entries(&path, "console", &mut order, at_ms - WINDOW_BEFORE_MS, at_ms + WINDOW_AFTER_MS, &mut entries);
        }
    }
    entries.sort_by_key(|e| (e.at, e.order));
    let skip = entries.len().saturating_sub(MAX_LOG_LINES);
    entries.iter().skip(skip)
        .map(|e| format!("{} {}", logfile::stamp(e.at, logfile::offset()), e.text))
        .collect()
}

/* ── Secrets ─────────────────────────────────────────────────────────── */

const SECRET_WORDS: [&str; 10] = [
    "api_key", "api-key", "apikey", "secret", "token", "password", "passwd", "authorization",
    "credential", "cookie",
];

fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_WORDS.iter().any(|word| name.contains(word))
        || name.contains("private_key") || name.contains("private-key") || name.ends_with("sid")
}

/// Assignment names worth scrubbing the value of in free text. Wider than
/// `secret_name`, which decides whole JSON fields.
fn secret_assignment(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["api_key", "api-key", "apikey", "secret", "token", "password", "passwd", "authorization",
     "client_id", "client-id", "clientid"].iter().any(|word| name.contains(word))
}

pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    pub fn new(mut secrets: Vec<String>) -> Self {
        secrets.retain(|s| !s.is_empty());
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        Redactor { secrets }
    }

    /// Values from .env that must never be written: anything under a secret
    /// name, and anything long enough to be a key rather than a port.
    pub fn from_env(root: &Path) -> Self {
        let mut pairs: Vec<(String, String)> = std::fs::read_to_string(root.join(".env"))
            .unwrap_or_default().lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.starts_with('#') { return None; }
                let (name, value) = line.split_once('=')?;
                let name = name.trim().trim_start_matches("export ").trim();
                Some((name.to_string(), value.trim().trim_matches(['"', '\'']).to_string()))
            })
            .collect();
        pairs.extend(std::env::vars().filter(|(name, _)| secret_name(name)));
        let secrets = pairs.into_iter().filter(|(name, value)| {
            let pathish = value.contains(['/', '\\', ' ']) && !secret_name(name);
            value.len() >= 6 && (secret_name(name) || (value.len() >= 16 && !pathish))
        }).map(|(_, value)| value).collect();
        Redactor::new(secrets)
    }

    pub fn text(&self, text: &str) -> String {
        let mut text = text.to_string();
        for secret in &self.secrets {
            if text.contains(secret.as_str()) {
                text = text.replace(secret.as_str(), "[redacted]");
            }
        }
        scrub_patterns(&text)
    }

    pub fn value(&self, value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(map.iter().map(|(key, v)| {
                let hide = secret_name(key) && match v {
                    Value::Null | Value::Bool(_) | Value::Object(_) | Value::Array(_) => false,
                    Value::String(s) => !s.is_empty(),
                    Value::Number(_) => true,
                };
                (key.clone(), if hide { Value::String("[redacted]".into()) } else { self.value(v) })
            }).collect()),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.value(v)).collect()),
            Value::String(s) => Value::String(self.text(s)),
            other => other.clone(),
        }
    }
}

fn token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn run(bytes: &[u8], from: usize, allowed: impl Fn(u8) -> bool) -> usize {
    bytes[from.min(bytes.len())..].iter().take_while(|b| allowed(**b)).count()
}

fn starts_with_ci(bytes: &[u8], at: usize, word: &str) -> bool {
    bytes.len() >= at + word.len() && bytes[at..at + word.len()].eq_ignore_ascii_case(word.as_bytes())
}

/// The same shapes radio/feedback.py scrubs, without a regex engine.
fn scrub_at(text: &str, i: usize) -> Option<(usize, String)> {
    let b = text.as_bytes();
    let word_start = i == 0 || !token_char(b[i - 1]);

    if text[i..].starts_with("-----BEGIN ") && text[i..].lines().next().is_some_and(|l| l.contains("PRIVATE KEY-----")) {
        let end = text[i + 11..].find("-----END ")
            .and_then(|at| {
                let from = i + 11 + at + 9;
                text[from..].find("-----").map(|close| from + close + 5)
            })
            .unwrap_or(text.len());
        return Some((end, "[redacted private key]".into()));
    }
    if !word_start {
        return None;
    }
    if text[i..].starts_with("sk-") {
        let n = run(b, i + 3, token_char);
        if n >= 8 { return Some((i + 3 + n, "sk-[redacted]".into())); }
    }
    if text[i..].starts_with("AIza") {
        let n = run(b, i + 4, token_char);
        if n >= 20 { return Some((i + 4 + n, "[redacted]".into())); }
    }
    if b.len() > i + 3 && b[i] == b'g' && b[i + 1] == b'h' && b"pousr".contains(&b[i + 2]) && b[i + 3] == b'_' {
        let n = run(b, i + 4, |c| c.is_ascii_alphanumeric());
        if n >= 20 { return Some((i + 4 + n, "[redacted]".into())); }
    }
    if b.len() > i + 4 && text[i..].starts_with("xox") && b"abpr".contains(&b[i + 3]) && b[i + 4] == b'-' {
        let n = run(b, i + 5, |c| c.is_ascii_alphanumeric() || c == b'-');
        if n >= 10 { return Some((i + 5 + n, "[redacted]".into())); }
    }
    for scheme in ["bearer", "basic"] {
        if starts_with_ci(b, i, scheme) && b.get(i + scheme.len()).is_some_and(|c| c.is_ascii_whitespace()) {
            let gap = run(b, i + scheme.len(), |c| c.is_ascii_whitespace());
            let from = i + scheme.len() + gap;
            let n = run(b, from, |c| c.is_ascii_alphanumeric() || b"._~+/=-".contains(&c));
            if n >= 8 { return Some((from + n, format!("{} [redacted]", &text[i..i + scheme.len()]))); }
        }
    }

    // name=value, name: value, "name": "value".
    let name_len = run(b, i, token_char);
    if name_len == 0 {
        return None;
    }
    let name = &text[i..i + name_len];
    let mut at = i + name_len;
    if name.eq_ignore_ascii_case("key") && b.get(at) == Some(&b'=') {
        // Only machine-made values: track keys are "artist|title".
        let n = run(b, at + 1, |c| token_char(c) || c == b'.');
        let end = at + 1 + n;
        if n >= 20 && b.get(end).is_none_or(|c| c.is_ascii_whitespace() || b"&\"',;".contains(c)) {
            return Some((end, format!("{}=[redacted]", name)));
        }
        return None;
    }
    if !secret_assignment(name) {
        return None;
    }
    at += run(b, at, |c| c == b'"' || c == b'\'');
    at += run(b, at, |c| c == b' ' || c == b'\t');
    if !matches!(b.get(at), Some(b':' | b'=')) {
        return None;
    }
    at += 1;
    at += run(b, at, |c| c == b' ' || c == b'\t');
    at += run(b, at, |c| c == b'"' || c == b'\'');
    if starts_with_ci(b, at, "bearer ") || starts_with_ci(b, at, "basic ") {
        return None; // the scheme's own rule takes it from here
    }
    let n = run(b, at, |c| !(c.is_ascii_whitespace() || b"\"'&,;}".contains(&c)));
    if n < 4 {
        return None;
    }
    Some((at + n, format!("{}[redacted]", &text[i..at])))
}

pub fn scrub_patterns(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut i, mut copied) = (0, 0);
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some((end, replacement)) = scrub_at(text, i) {
            out.push_str(&text[copied..i]);
            out.push_str(&replacement);
            i = end;
            copied = end;
            continue;
        }
        // Skip the rest of a word: every pattern begins at a word's start.
        let n = run(text.as_bytes(), i, token_char);
        i += n.max(1);
    }
    out.push_str(&text[copied..]);
    out
}

/* ── Plumbing ────────────────────────────────────────────────────────── */

pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(TABLE[(n >> shift & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn png(image: &egui::ColorImage) -> Option<Vec<u8>> {
    let [width, height] = image.size;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_array());
    }
    let buffer = image::RgbaImage::from_raw(width as u32, height as u32, rgba)?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    buffer.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    Some(bytes.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("defalt-reports-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        dir
    }

    const AT: i64 = 1_790_000_000_000;

    #[test]
    fn offline_reports_use_the_stations_layout() {
        let root = scratch("offline");
        std::fs::write(root.join("logs").join("console.log"), format!(
            "{} [console] station: failed: the station stopped\n{} [console] an hour ago\n",
            logfile::stamp(AT - 20_000, -18_000), logfile::stamp(AT - 3_600_000, 0))).unwrap();
        let report = Report {
            kind: "bug".into(), title: "Decks | froze".into(), description: "Both decks stopped.\nThen nothing.".into(),
            expected: "Music.".into(), attach_logs: true,
            context: json!({"station": {"health": "failed: the station stopped"}, "decks": [{"playing": false}]}),
            screenshot_png: Some(b"\x89PNG fake".to_vec()),
        };
        let filed = write_offline(&root, &report, AT, 7200, &Redactor::new(vec![])).unwrap();
        // 1_790_000_000 is 2026-09-21 14:13:20 UTC; +02:00 is 16:13:20.
        assert_eq!(filed.id, "20260921-161320-decks-froze");
        assert_eq!(filed.path, "reports/20260921-161320-decks-froze/");
        assert!(!filed.by_station);
        let folder = root.join("reports").join(&filed.id);
        let mut names: Vec<String> = std::fs::read_dir(&folder).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["context.json", "logs.txt", "report.md", "screenshot.png"]);

        let md = std::fs::read_to_string(folder.join("report.md")).unwrap();
        assert!(md.starts_with("# Bug: Decks / froze\n\n- id: 20260921-161320-decks-froze\n"));
        assert!(md.contains("- filed: 2026-09-21 16:13:20 (2026-09-21T16:13:20+02:00)\n"));
        assert!(md.contains("- status: open\n- from: console\n"));
        assert!(md.contains("- station: failed: the station stopped\n"));
        assert!(md.contains("## What happened\n\nBoth decks stopped.\nThen nothing.\n"));
        assert!(md.contains("## What I expected\n\nMusic.\n"));
        assert!(md.contains("- screenshot.png"));

        let context: Value = serde_json::from_slice(&std::fs::read(folder.join("context.json")).unwrap()).unwrap();
        assert_eq!(context["client"], "console");
        assert_eq!(context["console"]["decks"][0]["playing"], false);
        assert!(context["environment"]["version"].is_string());

        let logs = std::fs::read_to_string(folder.join("logs.txt")).unwrap();
        assert_eq!(logs.lines().count(), 1);
        assert!(logs.starts_with("2026-09-21T16:13:00.000+02:00 console [console] station: failed"));

        let inbox = std::fs::read_to_string(root.join("reports").join("INBOX.md")).unwrap();
        assert!(inbox.starts_with(INBOX_HEADER));
        assert!(inbox.ends_with("- [ ] 20260921-161320-decks-froze | 2026-09-21 16:13:20 | bug | open | Decks / froze | 20260921-161320-decks-froze/report.md\n"));

        let again = write_offline(&root, &report, AT, 7200, &Redactor::new(vec![])).unwrap();
        assert_eq!(again.id, "20260921-161320-decks-froze-2");
        let inbox = std::fs::read_to_string(root.join("reports").join("INBOX.md")).unwrap();
        assert_eq!(inbox.matches("# Feedback inbox").count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_python_inbox_header_is_the_same() {
        let python = include_str!("../radio/feedback.py");
        let start = python.find("INBOX_HEADER = \"\"\"").unwrap() + "INBOX_HEADER = \"\"\"".len();
        let end = start + python[start..].find("\"\"\"").unwrap();
        assert_eq!(&python[start..end], INBOX_HEADER.replace("\\\"", "\""));
    }

    #[test]
    fn logs_merge_across_rotations_and_drop_echoes() {
        let root = scratch("window");
        let logs = root.join("logs");
        std::fs::write(logs.join("station.log.1"), format!(
            "{} [out] too early\n{} [err] Traceback (most recent call last):\n  File \"x.py\"\nValueError: boom\n",
            logfile::stamp(AT - 1_200_000, -18_000), logfile::stamp(AT - 500_000, -18_000))).unwrap();
        std::fs::write(logs.join("station.log"), format!(
            "{} [out] after rotation\n{} [out] too late\n",
            logfile::stamp(AT - 100_000, 7200), logfile::stamp(AT + 120_000, 7200))).unwrap();
        std::fs::write(logs.join("console.log"), format!(
            "{} [station] after rotation\n{} [station:err] only the console saw this\n",
            logfile::stamp(AT - 99_500, 0), logfile::stamp(AT - 50_000, 0))).unwrap();
        std::fs::write(logs.join("notes.txt"), "not a log\n").unwrap();
        let lines = log_window(&logs, AT, 0);
        let texts: Vec<&str> = lines.iter().map(|l| l.split_once(' ').unwrap().1).collect();
        assert_eq!(texts, [
            "station [err] Traceback (most recent call last):",
            "station   File \"x.py\"",
            "station ValueError: boom",
            "station [out] after rotation",
            "console [station:err] only the console saw this",
        ]);
        let excerpt = console_excerpt(&logs, AT);
        assert_eq!(excerpt.len(), 2);
        assert!(excerpt[0].ends_with(" [station] after rotation"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn secrets_are_scrubbed_but_track_keys_survive() {
        let root = scratch("secrets");
        std::fs::write(root.join(".env"), "OPENROUTER_API_KEY=abcd1234efgh5678\nPORT=8090\nHOST=127.0.0.1\n\
            SPOTIFY_CLIENT_SECRET='shh-its-a-secret'\n").unwrap();
        let scrub = Redactor::from_env(&root);
        let text = scrub.text("key abcd1234efgh5678 used; sk-or-v1-0123456789abcdef; Authorization: Bearer eyJhbGciOi.xyz; \
            ?api_key=zzzzzzzz&key=AbCdEfGhIjKlMnOpQrStUv&key=daft%20punk%7Cone; \
            {\"access_token\": \"tok-value-1\"} secret shh-its-a-secret on 127.0.0.1:8090 \
            -----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY----- done ghp_abcdefghijklmnopqrstuvwxyz");
        for leaked in ["abcd1234efgh5678", "0123456789abcdef", "eyJhbGciOi", "zzzzzzzz", "AbCdEfGhIjKlMnOpQrStUv",
                       "tok-value-1", "shh-its-a-secret", "MIIE", "ghp_abc"] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        assert!(text.contains("key=daft%20punk%7Cone"));
        assert!(text.contains("127.0.0.1:8090"));
        assert!(text.contains("Bearer [redacted]"));
        assert!(text.contains(" done "));
        assert_eq!(scrub.text("Ünïcödé text, all fine"), "Ünïcödé text, all fine");

        let value = Redactor::new(vec![]).value(&json!({
            "key": "daft punk|one", "client_secret": "abc", "nested": [{"token": 12}],
            "note": "Bearer abcdefghijkl", "llm": {"configured": true}}));
        assert_eq!(value["key"], "daft punk|one");
        assert_eq!(value["client_secret"], "[redacted]");
        assert_eq!(value["nested"][0]["token"], "[redacted]");
        assert_eq!(value["note"], "Bearer [redacted]");
        assert_eq!(value["llm"]["configured"], true);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_report_never_carries_a_secret() {
        let root = scratch("leak");
        std::fs::write(root.join("logs").join("console.log"),
            format!("{} [console] using sk-live-abcdefghijklmnop\n", logfile::stamp(AT - 1000, 0))).unwrap();
        let report = Report {
            kind: "suggestion".into(), title: "Leak".into(), description: "my key is sk-live-abcdefghijklmnop".into(),
            attach_logs: true, context: json!({"api_key": "sk-live-abcdefghijklmnop"}), ..Default::default()
        };
        let filed = write_offline(&root, &report, AT, 0, &Redactor::new(vec![])).unwrap();
        for entry in std::fs::read_dir(root.join("reports").join(&filed.id)).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains("abcdefghijklmnop"), "{text}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn slugs_and_base64_match_their_python_and_rfc_shapes() {
        assert_eq!(slug("Skip | lags!!"), "skip-lags");
        assert_eq!(slug("   "), "report");
        assert_eq!(slug("Ünïcode title"), "n-code-title");
        assert_eq!(slug(&"word ".repeat(20)), "word-word-word-word-word-word-word-word");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn an_unreachable_station_falls_back_to_writing_here() {
        let root = scratch("fallback");
        let report = Report { kind: "bug".into(), title: "Offline".into(), ..Default::default() };
        // Nothing listens on port 9; the post fails at once.
        let filed = file(&root, Some("http://127.0.0.1:9"), &report).unwrap();
        assert!(!filed.by_station);
        assert!(root.join("reports").join(&filed.id).join("report.md").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}
