//! Pulling a record off the internet and into the crate.
//!
//! The resolver is the station's, in Python, and it stays there: it knows how
//! to refuse a live take, a music video, a visualiser and a pitched
//! re-upload, and it took a lot of getting wrong to learn that. Reimplementing
//! it here would be a second copy to keep right.
//!
//! So this runs `python -m radio.pull` and reads the JSON it prints. That
//! makes pulling the one thing in the console that needs Python; everything
//! else works without it, and this says so plainly when it is missing rather
//! than failing at the moment you press the button.
//!
//! Where a record lands: your music folder, tagged, then imported the same
//! way any file already sitting there would be. Nothing here deletes it.
//!
//! Read NOTICE.md before using this at all.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

/// Where a pull has got to.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// Looking for something worth downloading.
    Resolving,
    /// Found one; downloading and levelling it.
    Fetching,
    /// On disk in your music folder; reading tempo, key and loudness.
    Analysing,
    Done { file: PathBuf, note: Option<String> },
    Failed { error: String },
}

impl Stage {
    pub fn is_over(&self) -> bool {
        matches!(self, Stage::Done { .. } | Stage::Failed { .. })
    }

    pub fn label(&self) -> String {
        match self {
            Stage::Resolving => "looking".into(),
            Stage::Fetching => "fetching".into(),
            Stage::Analysing => "analysing".into(),
            Stage::Done { note, .. } => note.clone().unwrap_or_else(|| "done".into()),
            Stage::Failed { error } => error.clone(),
        }
    }
}

pub struct Job {
    pub query: String,
    pub stage: Stage,
    /// Kept so the process can be stopped, and so it is reaped rather than
    /// left as a zombie when the window closes.
    child: Option<Child>,
    updates: Receiver<Stage>,
    pub started: std::time::Instant,
}

impl Job {
    /// Take whatever the worker has said since last time.
    pub fn poll(&mut self) {
        while let Ok(stage) = self.updates.try_recv() {
            self.stage = stage;
        }
        if self.stage.is_over() {
            if let Some(mut child) = self.child.take() {
                let _ = child.wait();
            }
            return;
        }
        // A worker that died without saying anything would otherwise sit at
        // "looking" forever.
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                self.child = None;
                if !self.stage.is_over() {
                    self.stage = Stage::Failed {
                        error: match status.code() {
                            Some(code) => format!("the puller exited with {code}"),
                            None => "the puller was killed".into(),
                        },
                    };
                }
            }
        }
    }

    pub fn cancel(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if !self.stage.is_over() {
            self.stage = Stage::Failed { error: "stopped".into() };
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Closing the window must not leave yt-dlp running.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The interpreter that can run the station, if there is one.
pub fn python(root: &Path) -> Option<PathBuf> {
    let venv = root.join(".venv").join("Scripts").join("python.exe");
    venv.is_file().then_some(venv)
}

pub fn start(root: &Path, query: &str, expected_ms: Option<u64>) -> Result<Job, String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err("nothing to look for".into());
    }
    let python = python(root).ok_or(
        "Pulling needs the station's Python environment, which is not set up here.",
    )?;

    let mut command = Command::new(python);
    command.arg("-m").arg("radio.pull").arg(&query);
    if let Some(ms) = expected_ms.filter(|ms| *ms > 0) {
        command.arg("--duration-ms").arg(ms.to_string());
    }
    let mut child = command
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start the puller: {error}"))?;

    let stdout = child.stdout.take().ok_or("the puller has no output")?;
    let (sender, updates) = channel();
    std::thread::spawn(move || read(stdout, sender));

    Ok(Job {
        query,
        stage: Stage::Resolving,
        child: Some(child),
        updates,
        started: std::time::Instant::now(),
    })
}

fn read(stdout: std::process::ChildStdout, sender: Sender<Stage>) {
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        // The library logs to stdout as well, so anything that is not one of
        // our objects is somebody else's business.
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        if let Some(stage) = parse(line) {
            let over = stage.is_over();
            if sender.send(stage).is_err() || over {
                return;
            }
        }
    }
}

/// Enough JSON for the five keys we emit.
///
/// A parser rather than a dependency: the shape is ours on both ends, and
/// serde would be a build's worth of crates for one flat object.
fn parse(line: &str) -> Option<Stage> {
    let stage = field(line, "stage")?;
    match stage.as_str() {
        "resolving" => Some(Stage::Resolving),
        "fetching" => Some(Stage::Fetching),
        "analysing" => Some(Stage::Analysing),
        "done" => Some(Stage::Done {
            file: PathBuf::from(field(line, "file")?),
            note: field(line, "note"),
        }),
        "failed" => Some(Stage::Failed {
            error: field(line, "error").unwrap_or_else(|| "it did not say why".into()),
        }),
        _ => None,
    }
}

/// One string value out of a flat JSON object, unescaped.
fn field(line: &str, name: &str) -> Option<String> {
    let needle = format!("\"{name}\"");
    let at = line.find(&needle)? + needle.len();
    let rest = line[at..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let mut chars = rest.strip_prefix('"')?.chars();

    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => {}
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(code)?);
                }
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}


/* ── Separation ──────────────────────────────────────────────────────── */

/// Where a record's four parts are, once they exist.
#[derive(Clone, Debug, PartialEq)]
pub struct Parts {
    pub drums: PathBuf,
    pub bass: PathBuf,
    pub harmonic: PathBuf,
    pub vocals: PathBuf,
}

impl Parts {
    /// In the order the engine expects them.
    pub fn in_order(&self) -> [&PathBuf; 4] {
        [&self.drums, &self.bass, &self.harmonic, &self.vocals]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Split {
    Working { device: String },
    Done { parts: Parts, cached: bool },
    Failed { error: String },
}

impl Split {
    pub fn is_over(&self) -> bool {
        matches!(self, Split::Done { .. } | Split::Failed { .. })
    }

    pub fn label(&self) -> String {
        match self {
            Split::Working { device } if device == "cuda" => "separating on the GPU".into(),
            Split::Working { .. } => "separating on the CPU, this is slow".into(),
            Split::Done { cached: true, .. } => "already separated".into(),
            Split::Done { .. } => "separated".into(),
            Split::Failed { error } => error.clone(),
        }
    }
}

pub struct Separation {
    pub deck: usize,
    pub stage: Split,
    child: Option<Child>,
    updates: Receiver<Split>,
    pub started: std::time::Instant,
}

impl Separation {
    pub fn poll(&mut self) {
        while let Ok(stage) = self.updates.try_recv() {
            self.stage = stage;
        }
        if self.stage.is_over() {
            if let Some(mut child) = self.child.take() {
                let _ = child.wait();
            }
            return;
        }
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                self.child = None;
                if !self.stage.is_over() {
                    self.stage = Split::Failed {
                        error: match status.code() {
                            Some(code) => format!("the separator exited with {code}"),
                            None => "the separator was killed".into(),
                        },
                    };
                }
            }
        }
    }
}

impl Drop for Separation {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn separate(root: &Path, deck: usize, audio: &Path) -> Result<Separation, String> {
    let python = python(root)
        .ok_or("Separation needs the station's Python environment, which is not set up here.")?;

    let mut child = Command::new(python)
        .arg("-m")
        .arg("radio.stems")
        .arg(audio)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start the separator: {error}"))?;

    let stdout = child.stdout.take().ok_or("the separator has no output")?;
    let (sender, updates) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let line = line.trim();
            if !line.starts_with('{') {
                continue;
            }
            if let Some(stage) = parse_split(line) {
                let over = stage.is_over();
                if sender.send(stage).is_err() || over {
                    return;
                }
            }
        }
    });

    Ok(Separation {
        deck,
        stage: Split::Working { device: String::new() },
        child: Some(child),
        updates,
        started: std::time::Instant::now(),
    })
}

fn parse_split(line: &str) -> Option<Split> {
    match field(line, "stage")?.as_str() {
        "separating" => Some(Split::Working {
            device: field(line, "device").unwrap_or_default(),
        }),
        "done" => Some(Split::Done {
            parts: Parts {
                drums: PathBuf::from(field(line, "drums")?),
                bass: PathBuf::from(field(line, "bass")?),
                // Demucs calls the harmonic part "other".
                harmonic: PathBuf::from(field(line, "other")?),
                vocals: PathBuf::from(field(line, "vocals")?),
            },
            cached: field(line, "cached").as_deref() == Some("true")
                || line.contains("\"cached\": true")
                || line.contains("\"cached\":true"),
        }),
        "failed" => Some(Split::Failed {
            error: field(line, "error").unwrap_or_else(|| "it did not say why".into()),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_done_line_carries_the_file() {
        let stage = parse(r#"{"stage": "done", "key": "a|b", "file": "C:\\x\\y.wav"}"#);
        match stage {
            Some(Stage::Done { file, .. }) => {
                assert_eq!(file, PathBuf::from(r"C:\x\y.wav"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_failure_keeps_its_reason() {
        let stage = parse(r#"{"stage": "failed", "error": "nothing usable found"}"#);
        assert_eq!(
            stage,
            Some(Stage::Failed { error: "nothing usable found".into() })
        );
    }

    #[test]
    fn a_failure_without_a_reason_still_reads_as_one() {
        match parse(r#"{"stage": "failed"}"#) {
            Some(Stage::Failed { error }) => assert!(!error.is_empty()),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn escapes_and_accents_survive() {
        // Titles arrive with both, and a mangled one is a mangled filename.
        let stage = parse(r#"{"stage":"done","file":"a\"b","note":"caf\u00e9"}"#);
        match stage {
            Some(Stage::Done { file, note }) => {
                assert_eq!(file, PathBuf::from("a\"b"));
                assert_eq!(note.as_deref(), Some("café"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn the_librarys_own_logging_is_not_mistaken_for_progress() {
        assert!(parse("ready Alex G - Pretend 360s").is_none());
        assert!(parse(r#"{"stage": "chatting"}"#).is_none());
    }

    #[test]
    fn only_the_last_two_stages_end_a_job() {
        assert!(!Stage::Resolving.is_over());
        assert!(!Stage::Fetching.is_over());
        assert!(!Stage::Analysing.is_over());
        assert!(Stage::Failed { error: "x".into() }.is_over());
        assert!(Stage::Done { file: PathBuf::new(), note: None }.is_over());
    }

    #[test]
    fn an_empty_query_is_refused_before_a_process_is_started() {
        let root = std::env::temp_dir();
        assert!(start(&root, "   ", None).is_err());
    }

    #[test]
    fn a_separation_maps_other_onto_harmonic() {
        // Demucs calls it "other"; a mixer calls it harmonic, and the engine
        // takes them in a fixed order.
        let line = r#"{"stage":"done","cached":false,"drums":"d.flac",
                       "bass":"b.flac","other":"o.flac","vocals":"v.flac"}"#;
        match parse_split(line) {
            Some(Split::Done { parts, cached }) => {
                assert!(!cached);
                assert_eq!(parts.harmonic, PathBuf::from("o.flac"));
                assert_eq!(
                    parts.in_order().map(|p| p.to_string_lossy().to_string()),
                    ["d.flac", "b.flac", "o.flac", "v.flac"]
                );
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn three_parts_is_not_a_separation() {
        let line = r#"{"stage":"done","drums":"d","bass":"b","vocals":"v"}"#;
        assert!(parse_split(line).is_none());
    }

    #[test]
    fn a_cached_separation_says_so() {
        let line = r#"{"stage":"done","cached":true,"drums":"d","bass":"b","other":"o","vocals":"v"}"#;
        match parse_split(line) {
            Some(Split::Done { cached, .. }) => assert!(cached),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn the_cpu_warns_and_the_gpu_does_not() {
        let gpu = Split::Working { device: "cuda".into() };
        let cpu = Split::Working { device: "cpu".into() };
        assert!(!gpu.label().contains("slow"));
        assert!(cpu.label().contains("slow"));
    }
}
