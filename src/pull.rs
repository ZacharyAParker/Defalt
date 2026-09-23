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
use std::process::{Child, Stdio};
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

/// What a worker's reader thread says: progress, or that the process has
/// gone and how. The reader reaps the child itself, so nothing on the UI
/// thread ever waits on one.
enum Update<T> {
    Stage(T),
    Exited(Option<i32>),
}

/// A worker process: its tree, for stopping it, and what it has said.
struct Worker<T> {
    /// Dropping the job -- the window closing -- kills the whole tree,
    /// yt-dlp and ffmpeg included.
    job: Option<crate::process::Job>,
    /// A child that could not be given a job is killed by id instead.
    pid: Option<u32>,
    updates: Receiver<Update<T>>,
}

impl<T: Send + 'static> Worker<T> {
    fn start(
        child: Child,
        job: Option<crate::process::Job>,
        parse: fn(&serde_json::Value) -> Option<T>,
        over: fn(&T) -> bool,
    ) -> Self {
        let pid = Some(child.id());
        let (sender, updates) = channel();
        std::thread::spawn(move || watch(child, sender, parse, over));
        Worker { job, pid, updates }
    }

    /// Everything said since last time. `Err(code)` once the process has
    /// exited, after whatever it said before it went.
    fn take(&mut self) -> (Vec<T>, Option<Option<i32>>) {
        let mut said = Vec::new();
        let mut exited = None;
        while let Ok(update) = self.updates.try_recv() {
            match update {
                Update::Stage(stage) => said.push(stage),
                Update::Exited(code) => {
                    exited = Some(code);
                    self.job = None;
                    self.pid = None;
                }
            }
        }
        (said, exited)
    }

    fn stop(&mut self) {
        match (self.job.take(), self.pid.take()) {
            (Some(job), _) => job.terminate(),
            (None, Some(pid)) => {
                let _ = crate::process::background("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .status();
            }
            _ => {}
        }
    }
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        // Closing the window must not leave yt-dlp running. The job's own
        // drop kills a contained tree; a bare child is killed by id.
        if self.job.is_none() {
            if let Some(pid) = self.pid.take() {
                let _ = crate::process::background("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .status();
            }
        }
    }
}

/// Read a worker's JSON lines until it closes its output, then reap it.
fn watch<T>(
    mut child: Child,
    sender: Sender<Update<T>>,
    parse: fn(&serde_json::Value) -> Option<T>,
    over: fn(&T) -> bool,
) {
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            // The library logs to stdout as well, so anything that is not one
            // of our objects is somebody else's business.
            let line = line.trim();
            if !line.starts_with('{') {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            if let Some(stage) = parse(&value) {
                let done = over(&stage);
                if sender.send(Update::Stage(stage)).is_err() || done {
                    break;
                }
            }
        }
    }
    let code = child.wait().ok().and_then(|status| status.code());
    let _ = sender.send(Update::Exited(code));
}

pub struct Job {
    pub query: String,
    pub stage: Stage,
    worker: Worker<Stage>,
    pub started: std::time::Instant,
}

impl Job {
    /// Take whatever the worker has said since last time.
    pub fn poll(&mut self) {
        let (said, exited) = self.worker.take();
        for stage in said {
            if !self.stage.is_over() {
                self.stage = stage;
            }
        }
        // A worker that died without saying anything would otherwise sit at
        // "looking" forever.
        if let Some(code) = exited {
            if !self.stage.is_over() {
                self.stage = Stage::Failed {
                    error: match code {
                        Some(code) => format!("the puller exited with {code}"),
                        None => "the puller was killed".into(),
                    },
                };
            }
        }
    }

    pub fn cancel(&mut self) {
        self.worker.stop();
        if !self.stage.is_over() {
            self.stage = Stage::Failed { error: "stopped".into() };
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

    let mut command = crate::process::background(python);
    command.arg("-m").arg("radio.pull").arg(&query);
    if let Some(ms) = expected_ms.filter(|ms| *ms > 0) {
        command.arg("--duration-ms").arg(ms.to_string());
    }
    command
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let (child, job) = crate::process::spawn_contained(&mut command)
        .map_err(|error| format!("could not start the puller: {error}"))?;

    Ok(Job {
        query,
        stage: Stage::Resolving,
        worker: Worker::start(child, job, parse, Stage::is_over),
        started: std::time::Instant::now(),
    })
}

fn text(value: &serde_json::Value, name: &str) -> Option<String> {
    value[name].as_str().map(str::to_string)
}

/// One progress line from `radio.pull`.
fn parse(value: &serde_json::Value) -> Option<Stage> {
    match value["stage"].as_str()? {
        "resolving" => Some(Stage::Resolving),
        "fetching" => Some(Stage::Fetching),
        "analysing" => Some(Stage::Analysing),
        "done" => Some(Stage::Done {
            file: PathBuf::from(text(value, "file")?),
            note: text(value, "note"),
        }),
        "failed" => Some(Stage::Failed {
            error: text(value, "error").unwrap_or_else(|| "it did not say why".into()),
        }),
        _ => None,
    }
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
    worker: Worker<Split>,
    pub started: std::time::Instant,
}

impl Separation {
    pub fn poll(&mut self) {
        let (said, exited) = self.worker.take();
        for stage in said {
            if !self.stage.is_over() {
                self.stage = stage;
            }
        }
        if let Some(code) = exited {
            if !self.stage.is_over() {
                self.stage = Split::Failed {
                    error: match code {
                        Some(code) => format!("the separator exited with {code}"),
                        None => "the separator was killed".into(),
                    },
                };
            }
        }
    }
}

pub fn separate(root: &Path, deck: usize, audio: &Path) -> Result<Separation, String> {
    let python = python(root)
        .ok_or("Separation needs the station's Python environment, which is not set up here.")?;

    let mut command = crate::process::background(python);
    command
        .arg("-m")
        .arg("radio.stems")
        .arg(audio)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let (child, job) = crate::process::spawn_contained(&mut command)
        .map_err(|error| format!("could not start the separator: {error}"))?;

    Ok(Separation {
        deck,
        stage: Split::Working { device: String::new() },
        worker: Worker::start(child, job, parse_split, Split::is_over),
        started: std::time::Instant::now(),
    })
}

fn parse_split(value: &serde_json::Value) -> Option<Split> {
    match value["stage"].as_str()? {
        "separating" => Some(Split::Working {
            device: text(value, "device").unwrap_or_default(),
        }),
        "done" => Some(Split::Done {
            parts: Parts {
                drums: PathBuf::from(text(value, "drums")?),
                bass: PathBuf::from(text(value, "bass")?),
                // Demucs calls the harmonic part "other".
                harmonic: PathBuf::from(text(value, "other")?),
                vocals: PathBuf::from(text(value, "vocals")?),
            },
            cached: value["cached"].as_bool().unwrap_or(false)
                || value["cached"].as_str() == Some("true"),
        }),
        "failed" => Some(Split::Failed {
            error: text(value, "error").unwrap_or_else(|| "it did not say why".into()),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> Option<Stage> {
        parse(&serde_json::from_str(text).ok()?)
    }

    fn split_line(text: &str) -> Option<Split> {
        parse_split(&serde_json::from_str(text).ok()?)
    }

    #[test]
    fn a_done_line_carries_the_file() {
        let stage = line(r#"{"stage": "done", "key": "a|b", "file": "C:\\x\\y.wav"}"#);
        match stage {
            Some(Stage::Done { file, .. }) => {
                assert_eq!(file, PathBuf::from(r"C:\x\y.wav"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_failure_keeps_its_reason() {
        let stage = line(r#"{"stage": "failed", "error": "nothing usable found"}"#);
        assert_eq!(
            stage,
            Some(Stage::Failed { error: "nothing usable found".into() })
        );
    }

    #[test]
    fn a_failure_without_a_reason_still_reads_as_one() {
        match line(r#"{"stage": "failed"}"#) {
            Some(Stage::Failed { error }) => assert!(!error.is_empty()),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn escapes_and_accents_survive() {
        // Titles arrive with both, and a mangled one is a mangled filename.
        let stage = line(r#"{"stage":"done","file":"a\"b","note":"caf\u00e9"}"#);
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
        assert!(line("ready Alex G - Pretend 360s").is_none());
        assert!(line(r#"{"stage": "chatting"}"#).is_none());
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
        let text = r#"{"stage":"done","cached":false,"drums":"d.flac",
                       "bass":"b.flac","other":"o.flac","vocals":"v.flac"}"#;
        match split_line(text) {
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
        let text = r#"{"stage":"done","drums":"d","bass":"b","vocals":"v"}"#;
        assert!(split_line(text).is_none());
    }

    #[test]
    fn a_cached_separation_says_so() {
        let text = r#"{"stage":"done","cached":true,"drums":"d","bass":"b","other":"o","vocals":"v"}"#;
        match split_line(text) {
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

    #[cfg(windows)]
    #[test]
    fn a_worker_is_reaped_off_the_ui_thread_and_its_silence_is_a_failure() {
        let mut command = crate::process::background("cmd");
        command.args(["/C", "echo", "not json"]).stdout(Stdio::piped()).stdin(Stdio::null());
        let (child, job) = crate::process::spawn_contained(&mut command).unwrap();
        let mut job = Job {
            query: "x".into(),
            stage: Stage::Resolving,
            worker: Worker::start(child, job, parse, Stage::is_over),
            started: std::time::Instant::now(),
        };
        let began = std::time::Instant::now();
        while !job.stage.is_over() && began.elapsed().as_secs() < 10 {
            let polled = std::time::Instant::now();
            job.poll();
            assert!(polled.elapsed().as_millis() < 50, "poll waited on the child");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(matches!(job.stage, Stage::Failed { .. }), "{:?}", job.stage);
    }
}
