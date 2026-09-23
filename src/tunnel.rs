//! Remote listening: the Cloudflare tunnel, and the broadcast behind it.
//!
//! The tunnel is only up while the radio is: it starts once the station the
//! console started has answered, and it is the first thing stopped when the
//! radio stops or the console closes. It only ever starts with Cloudflare
//! Access configured -- the station refuses remote hosts without it, and a
//! tunnel to a station that refuses everything is a tunnel for nothing.
//!
//! `Remote` is what the console holds: the loopback stream server (started
//! with the engine, idle until somebody listens), the tunnel, and the
//! station's remote settings, read from its /api/remote/status.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::broadcast::{self, Broadcast};
use crate::engine::Telemetry;
use crate::station::Station;

/// What cloudflared prints for each of its (usually four) edge connections.
const REGISTERED: &str = "Registered tunnel connection";
const UNREGISTERED: [&str; 3] = ["Unregistered tunnel connection", "Connection terminated", "Retrying connection"];
const DEFAULT_BIN: &str = r"C:\Program Files (x86)\cloudflared\cloudflared.exe";
/// How often the station's remote settings are read while it is up.
const SETTINGS_EVERY: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// .env, then the process environment on top -- python-dotenv's rule, so the
/// console and the station always agree on a value.
pub fn read_env(root: &Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    for line in std::fs::read_to_string(root.join(".env")).unwrap_or_default().lines() {
        let line = line.trim();
        if line.starts_with('#') { continue; }
        let Some((name, value)) = line.split_once('=') else { continue };
        let name = name.trim().trim_start_matches("export ").trim();
        env.insert(name.to_string(), value.trim().trim_matches(['"', '\'']).to_string());
    }
    for (name, value) in std::env::vars() {
        if !value.is_empty() { env.insert(name, value); }
    }
    env
}

#[derive(Clone, Debug, PartialEq)]
pub struct TunnelConfig {
    pub name: String,
    pub bin: PathBuf,
    pub config: PathBuf,
}

/// Whether, and how, to run the tunnel. An Err says why not in words for the
/// toolbar; `Ok` is only possible with every Access setting present.
pub fn tunnel_config(env: &HashMap<String, String>, exists: impl Fn(&Path) -> bool) -> Result<TunnelConfig, String> {
    let get = |name: &str| env.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());
    let name = get("REMOTE_TUNNEL").ok_or("no REMOTE_TUNNEL in .env")?.to_string();
    // Fail closed: without Access the station would refuse every remote
    // request anyway, and without REMOTE_HOSTS it would not know its name.
    for required in ["REMOTE_HOSTS", "CF_ACCESS_TEAM_DOMAIN", "CF_ACCESS_AUD"] {
        if get(required).is_none() {
            return Err(format!("{required} is not set; the tunnel stays down"));
        }
    }
    let bin = match get("CLOUDFLARED_BIN") {
        Some(bin) => PathBuf::from(bin),
        None => on_path("cloudflared", env.get("PATH").map(String::as_str).unwrap_or(""), &exists)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_BIN)),
    };
    let config = match get("CLOUDFLARED_CONFIG") {
        Some(config) => PathBuf::from(config),
        None => {
            let home = get("USERPROFILE").or(get("HOME")).ok_or("no home folder for the cloudflared config")?;
            PathBuf::from(home).join(".cloudflared").join(format!("{name}.yml"))
        }
    };
    if !exists(&bin) {
        return Err(format!("cloudflared not found ({})", bin.display()));
    }
    if !exists(&config) {
        return Err(format!("no tunnel config at {}", config.display()));
    }
    Ok(TunnelConfig { name, bin, config })
}

fn on_path(program: &str, path: &str, exists: &impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let separator = if cfg!(windows) { ';' } else { ':' };
    path.split(separator).filter(|dir| !dir.trim().is_empty()).find_map(|dir| {
        [format!("{program}.exe"), program.to_string()].into_iter()
            .map(|file| Path::new(dir.trim()).join(file))
            .find(|candidate| exists(candidate))
    })
}

pub fn command_args(config: &TunnelConfig) -> Vec<String> {
    vec!["tunnel".into(), "--config".into(), config.config.display().to_string(),
         "run".into(), config.name.clone()]
}

#[derive(Clone, Debug, PartialEq)]
pub enum TunnelState {
    Off,
    Connecting,
    Online(usize),
}

impl TunnelState {
    pub fn word(&self) -> &'static str {
        match self { TunnelState::Off => "off", TunnelState::Connecting => "connecting", TunnelState::Online(_) => "online" }
    }
}

/// Follow cloudflared's log: which lines move the connection count.
pub fn follow(state: &TunnelState, line: &str) -> TunnelState {
    let connections = match state { TunnelState::Online(n) => *n, _ => 0 };
    if line.contains(REGISTERED) {
        TunnelState::Online(connections + 1)
    } else if UNREGISTERED.iter().any(|marker| line.contains(marker)) && connections > 0 {
        if connections > 1 { TunnelState::Online(connections - 1) } else { TunnelState::Connecting }
    } else {
        state.clone()
    }
}

struct Tunnel {
    child: Option<Child>,
    job: Option<crate::process::Job>,
    state: Arc<Mutex<TunnelState>>,
    /// Bumped per process, so a dead one's log thread cannot move the state.
    generation: Arc<AtomicU64>,
    started: Instant,
    retry_at: Instant,
    backoff: Duration,
}

impl Default for Tunnel {
    fn default() -> Self {
        Tunnel {
            child: None,
            job: None,
            state: Arc::new(Mutex::new(TunnelState::Off)),
            generation: Arc::new(AtomicU64::new(0)),
            started: Instant::now(),
            retry_at: Instant::now(),
            backoff: Duration::from_secs(2),
        }
    }
}

impl Tunnel {
    fn state(&self) -> TunnelState {
        self.state.lock().map(|s| s.clone()).unwrap_or(TunnelState::Off)
    }

    fn set(&self, state: TunnelState) {
        if let Ok(mut slot) = self.state.lock() { *slot = state; }
    }

    fn tick(&mut self, config: Option<&TunnelConfig>) {
        let Some(config) = config else {
            self.stop();
            return;
        };
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(exit)) = child.try_wait() {
                self.child = None;
                self.job = None;
                self.generation.fetch_add(1, Ordering::AcqRel);
                if self.started.elapsed() > Duration::from_secs(120) { self.backoff = Duration::from_secs(2); }
                crate::logfile::log!("tunnel: cloudflared exited ({exit}); retrying in {}s", self.backoff.as_secs());
                self.retry_at = Instant::now() + self.backoff;
                self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
                self.set(TunnelState::Connecting);
            }
            return;
        }
        if Instant::now() < self.retry_at {
            return;
        }
        self.start(config);
    }

    fn start(&mut self, config: &TunnelConfig) {
        let mut command = crate::process::background(&config.bin);
        command.args(command_args(config))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, job) = match crate::process::spawn_contained(&mut command) {
            Ok(spawned) => spawned,
            Err(error) => {
                crate::logfile::log!("tunnel: could not start cloudflared: {error}");
                self.retry_at = Instant::now() + self.backoff;
                self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
                self.set(TunnelState::Connecting);
                return;
            }
        };
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let streams: [Option<Box<dyn std::io::Read + Send>>; 2] = [
            child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ];
        for stream in streams.into_iter().flatten() {
            let (state, current) = (self.state.clone(), self.generation.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    crate::logfile::write("tunnel", line);
                    if current.load(Ordering::Acquire) != generation { continue; }
                    if let Ok(mut state) = state.lock() {
                        let next = follow(&state, line);
                        *state = next;
                    }
                }
            });
        }
        crate::logfile::write("tunnel", &format!("starting cloudflared tunnel {}", config.name));
        self.started = Instant::now();
        self.child = Some(child);
        self.job = job;
        self.set(TunnelState::Connecting);
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            self.generation.fetch_add(1, Ordering::AcqRel);
            crate::process::kill(&mut child, self.job.as_ref());
            let _ = child.wait();
            self.job = None;
            crate::logfile::write("tunnel", "cloudflared stopped");
        }
        self.backoff = Duration::from_secs(2);
        self.retry_at = Instant::now();
        self.set(TunnelState::Off);
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The station's remote settings (station.yaml `remote.*`), as it reports them.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub mute_local: bool,
}

pub fn settings_from(body: &serde_json::Value) -> Option<Settings> {
    Some(Settings {
        enabled: body.get("enabled")?.as_bool().unwrap_or(true),
        mute_local: body["mute_local"].as_bool().unwrap_or(false),
    })
}

/// Whether the tunnel should be up right now.
pub fn wanted(station_ours: bool, station_live: bool, settings: Option<&Settings>, config: &Result<TunnelConfig, String>) -> bool {
    station_ours && station_live && config.is_ok() && settings.is_some_and(|s| s.enabled)
}

pub struct Remote {
    pub broadcast: Option<Broadcast>,
    tunnel: Tunnel,
    config: Option<Result<TunnelConfig, String>>,
    settings: Arc<Mutex<Option<Settings>>>,
    asked: Option<Instant>,
    bind_retry: Option<Instant>,
    root: PathBuf,
}

impl Default for Remote {
    fn default() -> Self {
        Remote { broadcast: None, tunnel: Tunnel::default(), config: None, settings: Arc::new(Mutex::new(None)),
                 asked: None, bind_retry: None, root: PathBuf::new() }
    }
}

impl Remote {
    /// Once a frame, from the app's update.
    pub fn tick(&mut self, root: &Path, telemetry: Option<&Arc<Telemetry>>, station: &Station) {
        if self.root != root || self.config.is_none() {
            self.root = root.to_path_buf();
            let env = read_env(root);
            self.config = Some(tunnel_config(&env, |path| path.is_file()));
        }
        if self.broadcast.is_none() {
            if let Some(telemetry) = telemetry {
                if self.bind_retry.map_or(true, |at| Instant::now() >= at) {
                    match Broadcast::start(broadcast::Config::from_env(root), Some(telemetry.clone())) {
                        Ok(started) => self.broadcast = Some(started),
                        Err(error) => {
                            crate::logfile::log!("{error}");
                            self.bind_retry = Some(Instant::now() + Duration::from_secs(15));
                        }
                    }
                }
            }
        }

        let live = station.ready();
        if station.ours() && live {
            if self.asked.map_or(true, |at| at.elapsed() >= SETTINGS_EVERY) {
                self.asked = Some(Instant::now());
                let (url, slot) = (format!("{}/api/remote/status", station.url()), self.settings.clone());
                std::thread::spawn(move || {
                    let read = ureq::get(&url).call().ok()
                        .and_then(|mut response| response.body_mut().read_json::<serde_json::Value>().ok());
                    if let Some(settings) = read.as_ref().and_then(settings_from) {
                        if let Ok(mut slot) = slot.lock() { *slot = Some(settings); }
                    }
                });
            }
        } else {
            self.asked = None;
            if let Ok(mut slot) = self.settings.lock() { *slot = None; }
        }
        let settings = self.settings.lock().ok().and_then(|s| s.clone());

        let config = self.config.clone().unwrap_or(Err(String::new()));
        let want = wanted(station.ours(), live, settings.as_ref(), &config);
        self.tunnel.tick(if want { config.as_ref().ok() } else { None });

        if let Some(broadcast) = &self.broadcast {
            broadcast.set_mute_local(settings.as_ref().is_some_and(|s| s.mute_local));
            let state = self.tunnel.state();
            broadcast.set_extra("tunnel", serde_json::json!({
                "state": state.word(),
                "connections": match state { TunnelState::Online(n) => n, _ => 0 },
                "configured": config.is_ok(),
                "note": config.as_ref().err(),
            }));
        }
    }

    /// Before the station goes down: the tunnel first, so nothing reaches a
    /// station that is shutting down.
    pub fn stop_tunnel(&mut self) {
        self.tunnel.stop();
        if let Ok(mut slot) = self.settings.lock() { *slot = None; }
        self.asked = None;
    }

    pub fn tunnel_state(&self) -> TunnelState {
        self.tunnel.state()
    }

    /// Whether remote listening is set up at all, and if not, why.
    pub fn configured(&self) -> Result<(), String> {
        match &self.config {
            Some(Ok(_)) => Ok(()),
            Some(Err(why)) => Err(why.clone()),
            None => Err(String::new()),
        }
    }

    pub fn listeners(&self) -> usize {
        self.broadcast.as_ref().map_or(0, |b| b.status().listeners)
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // The tunnel before the server, like everything else here.
        self.tunnel.stop();
        self.broadcast = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    const FULL: &[(&str, &str)] = &[
        ("REMOTE_TUNNEL", "defalt"), ("REMOTE_HOSTS", "radio.example.org"),
        ("CF_ACCESS_TEAM_DOMAIN", "team.cloudflareaccess.com"), ("CF_ACCESS_AUD", "abc"),
        ("USERPROFILE", r"C:\Users\someone"), ("PATH", r"C:\Windows;C:\Tools"),
    ];

    #[test]
    fn the_command_runs_the_named_tunnel_with_its_config() {
        let config = TunnelConfig { name: "defalt".into(), bin: "cloudflared".into(),
                                    config: PathBuf::from(r"C:\Users\someone\.cloudflared\defalt.yml") };
        assert_eq!(command_args(&config), vec!["tunnel", "--config", r"C:\Users\someone\.cloudflared\defalt.yml",
                                               "run", "defalt"]);
    }

    #[test]
    fn paths_default_to_path_then_program_files_and_the_users_cloudflared_folder() {
        let on_path = PathBuf::from(r"C:\Tools").join("cloudflared.exe");
        let config = tunnel_config(&env(FULL), |p| p == on_path || p.ends_with("defalt.yml")).unwrap();
        assert_eq!(config.bin, on_path);
        assert_eq!(config.config, PathBuf::from(r"C:\Users\someone").join(".cloudflared").join("defalt.yml"));

        let config = tunnel_config(&env(FULL), |p| p == Path::new(DEFAULT_BIN) || p.ends_with("defalt.yml")).unwrap();
        assert_eq!(config.bin, PathBuf::from(DEFAULT_BIN));

        let mut explicit = env(FULL);
        explicit.insert("CLOUDFLARED_BIN".into(), r"D:\cf\cloudflared.exe".into());
        explicit.insert("CLOUDFLARED_CONFIG".into(), r"D:\cf\other.yml".into());
        let config = tunnel_config(&explicit, |_| true).unwrap();
        assert_eq!(config.bin, PathBuf::from(r"D:\cf\cloudflared.exe"));
        assert_eq!(config.config, PathBuf::from(r"D:\cf\other.yml"));
    }

    #[test]
    fn it_fails_closed_without_access() {
        assert!(tunnel_config(&env(&FULL[1..]), |_| true).unwrap_err().contains("REMOTE_TUNNEL"));
        for missing in ["REMOTE_HOSTS", "CF_ACCESS_TEAM_DOMAIN", "CF_ACCESS_AUD"] {
            let mut partial = env(FULL);
            partial.insert(missing.into(), "  ".into());
            let error = tunnel_config(&partial, |_| true).unwrap_err();
            assert!(error.contains(missing), "{error}");
        }
        assert!(tunnel_config(&env(FULL), |_| false).unwrap_err().contains("cloudflared not found"));
        let missing_config = tunnel_config(&env(FULL), |p| !p.ends_with("defalt.yml")).unwrap_err();
        assert!(missing_config.contains("no tunnel config"), "{missing_config}");
    }

    #[test]
    fn the_tunnel_is_only_wanted_while_our_station_is_up_and_remote_is_on() {
        let ok: Result<TunnelConfig, String> = Ok(TunnelConfig { name: "t".into(), bin: "b".into(), config: "c".into() });
        let on = Settings { enabled: true, mute_local: false };
        let off = Settings { enabled: false, mute_local: false };
        assert!(wanted(true, true, Some(&on), &ok));
        assert!(!wanted(false, true, Some(&on), &ok), "an adopted station is not ours to publish");
        assert!(!wanted(true, false, Some(&on), &ok), "not before the station answers");
        assert!(!wanted(true, true, None, &ok), "not before its remote settings are known");
        assert!(!wanted(true, true, Some(&off), &ok));
        assert!(!wanted(true, true, Some(&on), &Err("no access".into())));
    }

    #[test]
    fn registered_connections_are_counted_from_the_log() {
        let mut state = TunnelState::Connecting;
        state = follow(&state, "2026-01-01T00:00:00Z INF Starting tunnel tunnelID=abc");
        assert_eq!(state, TunnelState::Connecting);
        state = follow(&state, "INF Registered tunnel connection connIndex=0 location=ord01");
        state = follow(&state, "INF Registered tunnel connection connIndex=1 location=ord02");
        assert_eq!(state, TunnelState::Online(2));
        state = follow(&state, "WRN Connection terminated connIndex=0");
        assert_eq!(state, TunnelState::Online(1));
        state = follow(&state, "INF Unregistered tunnel connection connIndex=1");
        assert_eq!(state, TunnelState::Connecting);
        assert_eq!(state.word(), "connecting");
    }

    #[test]
    fn settings_are_read_from_the_stations_remote_status() {
        let body = serde_json::json!({"enabled": false, "mute_local": true, "tunnel": {}});
        assert_eq!(settings_from(&body), Some(Settings { enabled: false, mute_local: true }));
        assert_eq!(settings_from(&serde_json::json!({"error": "nope"})), None);
    }
}
