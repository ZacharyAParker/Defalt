//! The radio station, from the console's side.
//!
//! Side Room is a Python process: it picks records, writes what the hosts say,
//! renders their voices and schedules the whole thing on an hour clock. None
//! of that belongs in Rust, and none of it is going to be rewritten here.
//!
//! What this does is run it, watch it, and stop it. The console starts the
//! station as a child, polls `/api/status`, and shows what is on air.
//!
//! Playing it is `airtime`'s job, not this one's. This starts the station,
//! watches it and stops it; that reads the schedule it publishes and puts it
//! on the console's output.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// What the station says about itself.
#[derive(Clone, Debug, Default)]
pub struct OnAir {
    pub state: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub note: Option<String>,
    /// The last few things the hosts said, newest last.
    pub transcript: Vec<TranscriptLine>,
    pub mix_config: serde_json::Value,
    pub vibe: Option<String>,
    pub track_key: String,
    pub position: f64,
    pub duration: f64,
}

#[derive(Clone, Debug)]
pub struct TranscriptLine {
    pub host: String,
    pub text: String,
    pub start_at: f64,
    pub active: bool,
    pub source: Option<String>,
    pub source_url: Option<String>,
}

pub enum Health {
    /// Not running, and we did not start it.
    Off,
    /// Started; waiting for it to answer.
    Starting,
    Live(Box<OnAir>),
    Failed(String),
}

pub struct Station {
    root: PathBuf,
    port: u16,
    child: Option<Child>,
    /// True when the process on the port is not ours, so we must not kill it.
    adopted: bool,
    pub health: Health,
    polls: Receiver<Result<OnAir, String>>,
    sender: Sender<Result<OnAir, String>>,
    last_poll: Instant,
    inflight: bool,
    pub log: Vec<String>,
}

/// Often enough to feel live, rarely enough that it is not a load.
const POLL: Duration = Duration::from_millis(1200);

impl Station {
    pub fn new(root: &Path) -> Self {
        let (sender, polls) = channel();
        Station {
            root: root.to_path_buf(),
            port: port_of(root),
            child: None,
            adopted: false,
            health: Health::Off,
            polls,
            sender,
            last_poll: Instant::now() - POLL,
            inflight: false,
            log: Vec::new(),
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn running(&self) -> bool {
        !matches!(self.health, Health::Off | Health::Failed(_))
    }

    pub fn ours(&self) -> bool {
        self.child.is_some()
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.child.is_some() {
            return Ok(());
        }
        let python = crate::pull::python(&self.root)
            .ok_or("The station needs its Python environment, which is not set up here.")?;

        let mut child = crate::process::background(python)
            .arg("-m")
            .arg("radio")
            .current_dir(&self.root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start the station: {error}"))?;

        // Tie it to this process, or closing the window leaves a radio
        // playing to nobody with the port still held.
        leash(&child);

        // Its own logging is worth keeping: it says what it is downloading and
        // which model wrote a break.
        for stream in [
            child.stdout.take().map(Streams::Out),
            child.stderr.take().map(Streams::Err),
        ]
        .into_iter()
        .flatten()
        {
            let sender = self.sender.clone();
            std::thread::spawn(move || stream.pump(sender));
        }

        self.child = Some(child);
        self.adopted = false;
        self.health = Health::Starting;
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.health = Health::Off;
    }

    pub fn tick(&mut self) {
        while let Ok(message) = self.polls.try_recv() {
            match message {
                Ok(status) => {
                    self.inflight = false;
                    self.health = Health::Live(Box::new(status));
                }
                Err(line) if line.starts_with('\u{1}') => {
                    // A log line, tagged so it is not mistaken for an error.
                    self.log.push(line[1..].to_string());
                    if self.log.len() > 200 {
                        self.log.drain(..100);
                    }
                }
                Err(error) => {
                    self.inflight = false;
                    // While it is still coming up, silence is expected.
                    if !matches!(self.health, Health::Starting) {
                        self.health = Health::Failed(error);
                    }
                }
            }
        }

        // A station that died takes the view down with it rather than showing
        // a frozen last reading.
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                self.child = None;
                self.health = Health::Failed("the station stopped".into());
            }
        }

        if self.child.is_none() && !self.adopted && matches!(self.health, Health::Off) {
            return;
        }
        if self.inflight || self.last_poll.elapsed() < POLL {
            return;
        }
        self.last_poll = Instant::now();
        self.inflight = true;

        let url = format!("{}/api/status", self.url());
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(fetch(&url));
        });
    }

    /// Attach to a station somebody else already started.
    pub fn adopt(&mut self) {
        self.adopted = true;
        if matches!(self.health, Health::Off) {
            self.health = Health::Starting;
        }
    }
}

impl Drop for Station {
    fn drop(&mut self) {
        self.stop();
    }
}

enum Streams {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Streams {
    fn pump(self, sender: Sender<Result<OnAir, String>>) {
        let reader: Box<dyn BufRead> = match self {
            Streams::Out(stream) => Box::new(BufReader::new(stream)),
            Streams::Err(stream) => Box::new(BufReader::new(stream)),
        };
        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if sender.send(Err(format!("\u{1}{line}"))).is_err() {
                return;
            }
        }
    }
}

fn fetch(url: &str) -> Result<OnAir, String> {
    let body = ureq::get(url)
        .call()
        .map_err(|error| match error {
            ureq::Error::StatusCode(code) => format!("the station answered {code}"),
            _ => "the station is not answering".to_string(),
        })?
        .body_mut()
        .read_json::<serde_json::Value>()
        .map_err(|_| "the station sent something unreadable".to_string())?;

    Ok(on_air_from(&body))
}

pub fn on_air_from(body: &serde_json::Value) -> OnAir {
    let playing = &body["now_playing"];
    let transcript = body["transcript"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| {
                    Some(TranscriptLine {
                        host: line["host"].as_str().or(line["speaker"].as_str())?.to_string(),
                        text: line["text"].as_str()?.to_string(),
                        start_at: line["start_at"].as_f64().unwrap_or(0.0),
                        active: line["active"].as_bool().unwrap_or(false),
                        source: line["reference"]["source"].as_str().map(str::to_string),
                        source_url: line["reference"]["url"].as_str()
                            .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    OnAir {
        state: body["state"]
            .as_str()
            .or(body["note"].as_str())
            .unwrap_or("on air")
            .to_string(),
        title: playing["title"].as_str().map(str::to_string),
        artist: playing["artist"].as_str().map(str::to_string),
        note: body["note"].as_str().map(str::to_string),
        transcript,
        mix_config: body["mix_config"].clone(),
        vibe: body["vibe"]["description"].as_str().map(str::to_string),
        track_key: playing["key"].as_str().unwrap_or("").to_string(),
        position: playing["position"].as_f64().unwrap_or(0.0),
        duration: playing["duration"].as_f64().unwrap_or(0.0),
    }
}

/// The station reads PORT out of `.env`; we have to agree with it.
pub fn port_of(root: &Path) -> u16 {
    std::fs::read_to_string(root.join(".env"))
        .ok()
        .and_then(|text| {
            text.lines()
                .filter_map(|line| line.trim().split_once('='))
                .find(|(name, _)| name.trim() == "PORT")
                .and_then(|(_, value)| value.trim().parse().ok())
        })
        .unwrap_or(8090)
}

/// Tie a child, and everything it spawns, to this process.
///
/// The venv's python.exe is a launcher that re-execs into a second Python, so
/// killing the child we spawned would leave the actual server holding the
/// port. A job object closes over the whole tree, however this process dies.
#[cfg(windows)]
fn leash(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            return;
        }
        AssignProcessToJobObject(job, child.as_raw_handle() as _);
    }
}

#[cfg(not(windows))]
fn leash(_child: &Child) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_comes_from_the_projects_env() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // Whatever it is, it must match what the station will bind.
        let found = port_of(&root);
        let text = std::fs::read_to_string(root.join(".env")).unwrap_or_default();
        let expected: u16 = text
            .lines()
            .filter_map(|l| l.trim().split_once('='))
            .find(|(n, _)| n.trim() == "PORT")
            .and_then(|(_, v)| v.trim().parse().ok())
            .unwrap_or(8090);
        assert_eq!(found, expected);
    }

    #[test]
    fn a_missing_env_falls_back_rather_than_failing() {
        assert_eq!(port_of(&std::env::temp_dir().join("defalt-nothing-here")), 8090);
    }

    #[test]
    fn a_station_starts_off_and_is_not_ours() {
        let station = Station::new(&std::env::temp_dir());
        assert!(!station.running());
        assert!(!station.ours());
    }
}
