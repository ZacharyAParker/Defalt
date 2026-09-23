//! The remote stream: the console's own output, encoded and served.
//!
//! A phone in a car cannot run the browser mixer -- iOS suspends Web Audio on
//! lock, and Web Audio has no stems and no negative rate -- so the far end of
//! the tunnel hears the console itself. The engine's broadcast tap copies the
//! master (after the limiter, exactly what the speakers get) into a ring;
//! this pulls it out, pipes it through ffmpeg into AAC (or MP3), and fans the
//! bytes out to whoever has `GET /stream` open on a loopback port. The
//! station's `/listen` proxies that, behind its own auth.
//!
//! Nothing runs until somebody listens: the first listener starts the tap and
//! the encoder, and they stop half a minute after the last one leaves. While
//! anyone is listening the PC is asked not to sleep.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::engine::Telemetry;

pub const DEFAULT_PORT: u16 = 8091;
pub const DEFAULT_BITRATE: u32 = 160;
/// What the stream is resampled to, whatever the device runs at, so a device
/// change restarts the encoder without changing the stream under a player.
pub const STREAM_RATE: u32 = 48_000;
/// Recent audio handed to a new listener at once, for a fast start.
const BURST_SECONDS: f64 = 2.0;
/// The encoder stops this long after the last listener leaves.
const LINGER: Duration = Duration::from_secs(30);
/// Audio chunks a listener may fall behind before it is dropped. The encoder
/// emits roughly forty a second, so this is several seconds of patience.
const CLIENT_QUEUE: usize = 384;
/// No audio from the tap for this long (the device is gone) and the encoder
/// is fed silence, so players stay connected rather than stalling.
const QUIET: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Aac,
    Mp3,
}

impl Codec {
    pub fn parse(value: &str) -> Codec {
        if value.trim().eq_ignore_ascii_case("mp3") { Codec::Mp3 } else { Codec::Aac }
    }

    pub fn name(self) -> &'static str {
        match self { Codec::Aac => "aac", Codec::Mp3 => "mp3" }
    }

    pub fn content_type(self) -> &'static str {
        match self { Codec::Aac => "audio/aac", Codec::Mp3 => "audio/mpeg" }
    }

    /// Whether `bytes[0..2]` start a frame, so a listener joining mid-stream
    /// is handed a whole one first.
    fn syncs(self, bytes: &[u8]) -> bool {
        match (self, bytes) {
            // ADTS: twelve set bits, layer 00.
            (Codec::Aac, [0xFF, b, ..]) => b & 0xF6 == 0xF0,
            // MPEG audio: eleven set bits, then a version and layer that exist.
            (Codec::Mp3, [0xFF, b, ..]) => b & 0xE0 == 0xE0 && b & 0x18 != 0x08 && b & 0x06 != 0,
            _ => false,
        }
    }

    fn sync_offset(self, bytes: &[u8]) -> Option<usize> {
        (0..bytes.len().saturating_sub(1)).find(|&i| self.syncs(&bytes[i..]))
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub codec: Codec,
    pub bitrate_kbps: u32,
    pub ffmpeg: PathBuf,
    pub linger: Duration,
    pub client_queue: usize,
}

impl Config {
    /// BROADCAST_PORT, BROADCAST_CODEC, BROADCAST_BITRATE and FFMPEG_BIN, from
    /// the environment or the project's .env, the way the station reads them.
    pub fn from_env(root: &Path) -> Config {
        let env = crate::tunnel::read_env(root);
        let get = |name: &str| env.get(name).map(String::as_str).unwrap_or("");
        Config {
            port: get("BROADCAST_PORT").trim().parse().unwrap_or(DEFAULT_PORT),
            codec: Codec::parse(get("BROADCAST_CODEC")),
            bitrate_kbps: get("BROADCAST_BITRATE").trim().trim_end_matches(['k', 'K'])
                .parse().ok().filter(|b| (32..=320).contains(b)).unwrap_or(DEFAULT_BITRATE),
            ffmpeg: PathBuf::from(if get("FFMPEG_BIN").trim().is_empty() { "ffmpeg" } else { get("FFMPEG_BIN").trim() }),
            linger: LINGER,
            client_queue: CLIENT_QUEUE,
        }
    }

    fn burst_bytes(&self) -> usize {
        (self.bitrate_kbps as f64 * 1000.0 / 8.0 * BURST_SECONDS) as usize
    }
}

/// ffmpeg's arguments: raw stereo f32 at the device's rate in, one encoded
/// stream out, flushed per packet so the stream is not held in a buffer.
pub fn encoder_args(codec: Codec, bitrate_kbps: u32, input_rate: u32) -> Vec<String> {
    let mut args: Vec<String> = [
        "-hide_banner", "-nostdin", "-loglevel", "error",
        "-f", "f32le", "-ar", &input_rate.to_string(), "-ac", "2", "-i", "pipe:0",
    ].iter().map(|s| s.to_string()).collect();
    let (encoder, format) = match codec {
        Codec::Aac => (["-c:a", "aac", "-profile:a", "aac_low"].as_slice(), "adts"),
        Codec::Mp3 => (["-c:a", "libmp3lame"].as_slice(), "mp3"),
    };
    args.extend(encoder.iter().map(|s| s.to_string()));
    for arg in ["-b:a", &format!("{bitrate_kbps}k"), "-ar", &STREAM_RATE.to_string(), "-ac", "2",
                "-flush_packets", "1", "-f", format, "pipe:1"] {
        args.push(arg.to_string());
    }
    args
}

/// What the UI and the station's /api/remote/status show.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub encoding: bool,
    pub listeners: usize,
    pub uptime: f64,
    pub overruns: u64,
    pub bytes: u64,
    pub rate: u32,
    pub error: Option<String>,
}

struct Client {
    sender: SyncSender<Arc<[u8]>>,
    synced: bool,
}

pub struct Shared {
    config: Config,
    tap: Option<Arc<Telemetry>>,
    clients: Mutex<Vec<Client>>,
    burst: Mutex<(VecDeque<Arc<[u8]>>, usize)>,
    encoding: AtomicBool,
    closing: AtomicBool,
    listeners: AtomicUsize,
    /// Milliseconds since `born` when the last listener left (or joined).
    last_listener: AtomicU64,
    started: Mutex<Option<Instant>>,
    overruns: AtomicU64,
    bytes: AtomicU64,
    rate: AtomicU64,
    error: Mutex<Option<String>>,
    /// Anything else /status should say -- the tunnel sets its state here.
    extra: Mutex<serde_json::Value>,
    born: Instant,
}

impl Shared {
    fn since_born(&self) -> u64 {
        self.born.elapsed().as_millis() as u64
    }

    /// Hand encoded bytes to every listener, and keep them for the next one.
    pub fn publish(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let chunk: Arc<[u8]> = Arc::from(bytes);
        self.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        {
            let mut burst = self.burst.lock().unwrap_or_else(|p| p.into_inner());
            burst.1 += chunk.len();
            burst.0.push_back(chunk.clone());
            let limit = self.config.burst_bytes();
            while burst.1 > limit && burst.0.len() > 1 {
                if let Some(old) = burst.0.pop_front() { burst.1 -= old.len(); }
            }
        }
        let codec = self.config.codec;
        let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        clients.retain_mut(|client| {
            let piece = if client.synced {
                chunk.clone()
            } else {
                match codec.sync_offset(&chunk) {
                    Some(0) => chunk.clone(),
                    Some(at) => Arc::from(&chunk[at..]),
                    None => return true,
                }
            };
            match client.sender.try_send(piece) {
                Ok(()) => { client.synced = true; true }
                // Full: a slow listener is dropped, never waited for.
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
            }
        });
        self.set_listeners(clients.len());
    }

    fn set_listeners(&self, count: usize) {
        if self.listeners.swap(count, Ordering::AcqRel) != count || count > 0 {
            self.last_listener.store(self.since_born(), Ordering::Relaxed);
        }
    }

    /// A new listener: everything recent, from the first whole frame.
    fn join(&self) -> (Receiver<Arc<[u8]>>, f64) {
        let (sender, receiver) = sync_channel(self.config.client_queue.max(2));
        let recent: Vec<u8> = {
            let burst = self.burst.lock().unwrap_or_else(|p| p.into_inner());
            burst.0.iter().flat_map(|chunk| chunk.iter().copied()).collect()
        };
        let aligned = self.config.codec.sync_offset(&recent).map(|at| &recent[at..]).unwrap_or(&[]);
        let synced = !aligned.is_empty();
        if synced {
            let _ = sender.try_send(Arc::from(aligned));
        }
        let seconds = aligned.len() as f64 * 8.0 / (self.config.bitrate_kbps as f64 * 1000.0);
        let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        clients.push(Client { sender, synced });
        self.set_listeners(clients.len());
        (receiver, seconds)
    }

    /// Drop listeners whose connection is gone (their writer hung up).
    fn prune(&self) {
        let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        // A probe the writer thread never sees: zero bytes are skipped there.
        clients.retain(|client| !matches!(client.sender.try_send(Arc::from(&[][..])),
                                          Err(TrySendError::Disconnected(_))));
        self.set_listeners(clients.len());
    }

    pub fn status(&self) -> Status {
        Status {
            encoding: self.encoding.load(Ordering::Acquire),
            listeners: self.listeners.load(Ordering::Acquire),
            uptime: self.started.lock().ok().and_then(|s| *s).map_or(0.0, |s| s.elapsed().as_secs_f64()),
            overruns: self.overruns.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            rate: self.rate.load(Ordering::Relaxed) as u32,
            error: self.error.lock().ok().and_then(|e| e.clone()),
        }
    }

    fn status_json(&self) -> serde_json::Value {
        let status = self.status();
        let mut body = serde_json::json!({
            "encoding": status.encoding,
            "listeners": status.listeners,
            "codec": self.config.codec.name(),
            "content_type": self.config.codec.content_type(),
            "bitrate_kbps": self.config.bitrate_kbps,
            "uptime_seconds": (status.uptime * 10.0).round() / 10.0,
            "overruns": status.overruns,
            "bytes": status.bytes,
            "device_rate": status.rate,
            "mute_local": self.tap.as_ref().is_some_and(|t| t.broadcast.mute_local()),
            "error": status.error,
            "port": self.config.port,
        });
        if let (Some(body), Ok(extra)) = (body.as_object_mut(), self.extra.lock()) {
            if let Some(extra) = extra.as_object() {
                for (key, value) in extra { body.insert(key.clone(), value.clone()); }
            }
        }
        body
    }
}

/// A running stream server. Dropping it stops the encoder and the server.
pub struct Broadcast {
    pub shared: Arc<Shared>,
}

impl Broadcast {
    /// Bind the loopback port and start serving. The encoder waits for a
    /// listener. `tap` is None only in tests, where bytes are published by hand.
    pub fn start(config: Config, tap: Option<Arc<Telemetry>>) -> Result<Broadcast, String> {
        let listener = TcpListener::bind(("127.0.0.1", config.port))
            .map_err(|error| format!("broadcast: could not listen on 127.0.0.1:{}: {error}", config.port))?;
        let shared = Arc::new(Shared {
            config,
            tap,
            clients: Mutex::new(Vec::new()),
            burst: Mutex::new((VecDeque::new(), 0)),
            encoding: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            listeners: AtomicUsize::new(0),
            last_listener: AtomicU64::new(0),
            started: Mutex::new(None),
            overruns: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            rate: AtomicU64::new(0),
            error: Mutex::new(None),
            extra: Mutex::new(serde_json::json!({})),
            born: Instant::now(),
        });
        let server = shared.clone();
        std::thread::Builder::new()
            .name("defalt-broadcast".into())
            .spawn(move || serve(listener, server))
            .map_err(|error| format!("broadcast: {error}"))?;
        Ok(Broadcast { shared })
    }

    pub fn port(&self) -> u16 {
        self.shared.config.port
    }

    pub fn status(&self) -> Status {
        self.shared.status()
    }

    pub fn set_mute_local(&self, muted: bool) {
        if let Some(tap) = &self.shared.tap {
            tap.broadcast.set_mute_local(muted);
        }
    }

    /// Something for /status to carry besides the stream's own numbers.
    pub fn set_extra(&self, key: &str, value: serde_json::Value) {
        if let Ok(mut extra) = self.shared.extra.lock() {
            if let Some(extra) = extra.as_object_mut() {
                extra.insert(key.to_string(), value);
            }
        }
    }
}

impl Drop for Broadcast {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        // Wake the accept loop so it notices.
        let _ = TcpStream::connect_timeout(&([127, 0, 0, 1], self.shared.config.port).into(),
                                           Duration::from_millis(200));
        if let Ok(mut clients) = self.shared.clients.lock() { clients.clear(); }
    }
}

fn serve(listener: TcpListener, shared: Arc<Shared>) {
    for connection in listener.incoming() {
        if shared.closing.load(Ordering::Acquire) {
            return;
        }
        let Ok(connection) = connection else { continue };
        let shared = shared.clone();
        let _ = std::thread::Builder::new()
            .name("defalt-listener".into())
            .spawn(move || handle(connection, shared));
    }
}

fn respond(mut stream: TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.shutdown(Shutdown::Write);
}

/// The request line and headers, or None for anything that is not one.
fn read_head(stream: &mut TcpStream) -> Option<(String, String, Option<String>)> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?.take(8192));
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut host = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 { break; }
        let header = header.trim_end();
        if header.is_empty() { break; }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("host") { host = Some(value.trim().to_ascii_lowercase()); }
        }
    }
    Some((method, path, host))
}

fn handle(mut stream: TcpStream, shared: Arc<Shared>) {
    let Some((method, path, host)) = read_head(&mut stream) else { return };
    let port = shared.config.port;
    // Loopback names only: a page elsewhere must not rebind a name onto us.
    let allowed = [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")];
    if !host.as_deref().is_some_and(|h| allowed.iter().any(|a| a == h)) {
        return respond(stream, "403 Forbidden", "application/json", br#"{"error":"unexpected host"}"#);
    }
    if method != "GET" {
        return respond(stream, "405 Method Not Allowed", "application/json", br#"{"error":"GET only"}"#);
    }
    match path.split('?').next().unwrap_or("") {
        "/status" => {
            let body = shared.status_json().to_string();
            respond(stream, "200 OK", "application/json", body.as_bytes());
        }
        "/stream" => listen(stream, shared),
        _ => respond(stream, "404 Not Found", "application/json", br#"{"error":"not found"}"#),
    }
}

fn listen(mut stream: TcpStream, shared: Arc<Shared>) {
    let (receiver, burst) = shared.join();
    ensure_encoder(&shared);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\
         X-Burst-Seconds: {burst:.2}\r\nX-Stream-Bitrate: {}\r\n\r\n",
        shared.config.codec.content_type(), shared.config.bitrate_kbps);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_nodelay(true);
    if stream.write_all(head.as_bytes()).is_err() {
        drop(receiver);
        shared.prune();
        return;
    }
    // Blocks only this listener's own thread; the encoder never waits here.
    while let Ok(chunk) = receiver.recv() {
        if chunk.is_empty() { continue; }
        if stream.write_all(&chunk).is_err() { break; }
    }
    drop(receiver);
    let _ = stream.shutdown(Shutdown::Both);
    shared.prune();
}

fn ensure_encoder(shared: &Arc<Shared>) {
    if shared.tap.is_none() || shared.encoding.swap(true, Ordering::AcqRel) {
        return;
    }
    let pump = shared.clone();
    if std::thread::Builder::new()
        .name("defalt-encoder".into())
        .spawn(move || { encode(&pump); pump.encoding.store(false, Ordering::Release); })
        .is_err()
    {
        shared.encoding.store(false, Ordering::Release);
    }
}

struct Encoder {
    child: Child,
    job: Option<crate::process::Job>,
    stdin: ChildStdin,
    rate: u32,
}

impl Encoder {
    fn spawn(shared: &Arc<Shared>, rate: u32) -> Result<Encoder, String> {
        let config = &shared.config;
        let mut command = crate::process::background(&config.ffmpeg);
        command.args(encoder_args(config.codec, config.bitrate_kbps, rate))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, job) = crate::process::spawn_contained(&mut command)
            .map_err(|error| format!("could not start ffmpeg ({}): {error}", config.ffmpeg.display()))?;
        let stdin = child.stdin.take().ok_or("ffmpeg has no stdin")?;
        if let Some(mut stdout) = child.stdout.take() {
            let shared = shared.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 8192];
                while let Ok(read) = stdout.read(&mut buffer) {
                    if read == 0 { break; }
                    shared.publish(&buffer[..read]);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() { crate::logfile::write("broadcast", line.trim()); }
                }
            });
        }
        crate::logfile::write("broadcast", &format!(
            "encoding {} at {} kb/s from {rate} Hz", config.codec.name(), config.bitrate_kbps));
        Ok(Encoder { child, job, stdin, rate })
    }

    fn stop(mut self) {
        drop(self.stdin);
        crate::process::kill(&mut self.child, self.job.as_ref());
        let _ = self.child.wait();
    }
}

/// The encoder's life: from the first listener until they have all been gone
/// for `linger`.
fn encode(shared: &Arc<Shared>) {
    let Some(telemetry) = shared.tap.clone() else { return };
    let tap = &telemetry.broadcast;
    tap.start();
    let mut reader = tap.reader();
    *shared.started.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
    if let Ok(mut error) = shared.error.lock() { *error = None; }
    let mut encoder: Option<Encoder> = None;
    let mut samples: Vec<f32> = Vec::with_capacity(48_000);
    let mut bytes: Vec<u8> = Vec::with_capacity(48_000 * 4);
    let mut heard = Instant::now();
    let mut retry_at = Instant::now();
    let mut backoff = Duration::from_secs(1);
    let mut awake = false;
    loop {
        std::thread::sleep(Duration::from_millis(20));
        if shared.closing.load(Ordering::Acquire) { break; }
        let listeners = shared.listeners.load(Ordering::Acquire);
        if listeners == 0 {
            let idle = shared.since_born().saturating_sub(shared.last_listener.load(Ordering::Relaxed));
            if idle >= shared.config.linger.as_millis() as u64 { break; }
        }
        if (listeners > 0) != awake {
            awake = listeners > 0;
            stay_awake(awake);
        }

        samples.clear();
        let frames = reader.read(tap, &mut samples);
        shared.overruns.store(reader.overruns, Ordering::Relaxed);
        let rate = if tap.rate() > 0 { tap.rate() } else { telemetry.device_rate() };
        if rate == 0 { continue; }
        shared.rate.store(rate as u64, Ordering::Relaxed);
        if frames > 0 {
            heard = Instant::now();
        } else if heard.elapsed() >= QUIET {
            // The device went away; keep the players fed with silence.
            let gap = (heard.elapsed().as_secs_f64() * rate as f64) as usize;
            samples.resize(gap.min(rate as usize) * 2, 0.0);
            heard = Instant::now();
        }

        if encoder.as_ref().is_some_and(|e| e.rate != rate) {
            // The device changed rate: a new encoder at the new input rate.
            if let Some(old) = encoder.take() { old.stop(); }
        }
        if encoder.is_none() {
            if Instant::now() < retry_at { continue; }
            match Encoder::spawn(shared, rate) {
                Ok(fresh) => encoder = Some(fresh),
                Err(error) => {
                    crate::logfile::log!("broadcast: {error}");
                    if let Ok(mut slot) = shared.error.lock() { *slot = Some(error); }
                    retry_at = Instant::now() + backoff;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                    continue;
                }
            }
        }
        if samples.is_empty() { continue; }
        bytes.clear();
        for sample in &samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let Some(running) = encoder.as_mut() else { continue };
        if running.stdin.write_all(&bytes).is_err() {
            // ffmpeg died; start another after a moment.
            if let Some(dead) = encoder.take() { dead.stop(); }
            crate::logfile::write("broadcast", "the encoder stopped; restarting it");
            retry_at = Instant::now() + backoff;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        } else {
            backoff = Duration::from_secs(1);
        }
    }
    if let Some(encoder) = encoder.take() { encoder.stop(); }
    tap.stop();
    if awake { stay_awake(false); }
    *shared.started.lock().unwrap_or_else(|p| p.into_inner()) = None;
    if let Ok(mut burst) = shared.burst.lock() { *burst = (VecDeque::new(), 0); }
    crate::logfile::write("broadcast", "nobody is listening; the encoder is stopped");
}

/// Ask Windows not to sleep while somebody is listening. The request belongs
/// to the calling thread, which is the encoder's for as long as it runs.
#[cfg(windows)]
fn stay_awake(on: bool) {
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};
    unsafe {
        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
    }
}

#[cfg(not(windows))]
fn stay_awake(_on: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(port: u16, queue: usize) -> Config {
        Config { port, codec: Codec::Aac, bitrate_kbps: 160, ffmpeg: "ffmpeg".into(),
                 linger: Duration::from_secs(1), client_queue: queue }
    }

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn get(port: u16, path: &str) -> TcpStream {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").unwrap();
        stream
    }

    fn read_head_of(stream: &mut TcpStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        String::from_utf8(head).unwrap()
    }

    fn wait_for(what: impl Fn() -> bool) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if what() { return true; }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// An ADTS-looking frame: sync word first, then filler.
    fn frame(fill: u8, length: usize) -> Vec<u8> {
        let mut bytes = vec![fill; length];
        bytes[0] = 0xFF;
        bytes[1] = 0xF1;
        bytes
    }

    #[test]
    fn the_encoder_command_line_is_raw_pcm_in_and_one_stream_out() {
        let args = encoder_args(Codec::Aac, 160, 44_100).join(" ");
        assert!(args.contains("-f f32le -ar 44100 -ac 2 -i pipe:0"), "{args}");
        assert!(args.contains("-c:a aac -profile:a aac_low -b:a 160k -ar 48000 -ac 2"), "{args}");
        assert!(args.ends_with("-flush_packets 1 -f adts pipe:1"), "{args}");
        let mp3 = encoder_args(Codec::Mp3, 128, 96_000).join(" ");
        assert!(mp3.contains("-ar 96000") && mp3.contains("-c:a libmp3lame -b:a 128k"), "{mp3}");
        assert!(mp3.ends_with("-f mp3 pipe:1"), "{mp3}");
        assert_eq!(Codec::parse("MP3").content_type(), "audio/mpeg");
        assert_eq!(Codec::parse("").content_type(), "audio/aac");
    }

    #[test]
    fn config_reads_the_env_with_sane_fallbacks() {
        let root = std::env::temp_dir().join(format!("defalt-broadcast-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".env"), "BROADCAST_PORT=9123\nBROADCAST_CODEC=mp3\nBROADCAST_BITRATE=2000\n").unwrap();
        let config = Config::from_env(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(config.port, 9123);
        assert_eq!(config.codec, Codec::Mp3);
        assert_eq!(config.bitrate_kbps, DEFAULT_BITRATE, "an absurd bitrate falls back");
    }

    #[test]
    fn a_joining_listener_starts_on_a_whole_frame() {
        assert_eq!(Codec::Aac.sync_offset(&[0x12, 0xFF, 0x00, 0xFF, 0xF1, 0x50]), Some(3));
        assert_eq!(Codec::Mp3.sync_offset(&[0x00, 0xFF, 0xFB, 0x90]), Some(1));
        assert_eq!(Codec::Aac.sync_offset(&[0x00, 0x01]), None);
    }

    #[test]
    fn every_listener_gets_the_stream_and_a_slow_one_is_dropped() {
        let port = free_port();
        let broadcast = Broadcast::start(config(port, 16), None).unwrap();
        let shared = broadcast.shared.clone();
        // Audio from before anyone joined, some of it mid-frame.
        shared.publish(&[0x00, 0x11, 0x22]);
        shared.publish(&frame(0x01, 100));

        let mut fast = get(port, "/stream");
        let head = read_head_of(&mut fast);
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        assert!(head.contains("Content-Type: audio/aac") && head.contains("Cache-Control: no-store"), "{head}");
        assert!(!head.to_ascii_lowercase().contains("content-length"), "{head}");
        assert!(head.contains("X-Burst-Seconds: "), "{head}");
        let mut burst = vec![0u8; 100];
        fast.read_exact(&mut burst).unwrap();
        assert_eq!(burst, frame(0x01, 100), "the burst starts on the frame, not the junk before it");

        let slow = get(port, "/stream");
        assert!(wait_for(|| shared.status().listeners == 2));

        shared.publish(&frame(0x02, 50));
        let mut second = vec![0u8; 50];
        fast.read_exact(&mut second).unwrap();
        assert_eq!(second, frame(0x02, 50));

        // The fast one keeps reading; the slow one never does.
        let reading = std::thread::spawn(move || {
            let mut sink = vec![0u8; 1 << 16];
            let mut total = 0usize;
            fast.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            while let Ok(read) = fast.read(&mut sink) {
                if read == 0 { break; }
                total += read;
            }
            total
        });
        let big = frame(0x03, 1 << 18);
        let mut published = 0usize;
        for _ in 0..600 {
            shared.publish(&big);
            published += big.len();
            std::thread::sleep(Duration::from_millis(2));
            if shared.status().listeners < 2 { break; }
        }
        assert!(wait_for(|| shared.status().listeners == 1), "the slow listener was never dropped");
        assert!(published < 600 * big.len(), "publishing never noticed the slow listener");
        drop(slow);
        // The fast one is still being fed.
        shared.publish(&frame(0x04, 10));
        drop(broadcast);
        assert!(reading.join().unwrap() > 0);
    }

    #[test]
    fn status_is_json_and_strangers_are_refused() {
        let port = free_port();
        let broadcast = Broadcast::start(config(port, 8), None).unwrap();
        broadcast.set_extra("tunnel", serde_json::json!({"state": "online"}));
        let mut stream = get(port, "/status");
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        let body: serde_json::Value = serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["listeners"], 0);
        assert_eq!(body["codec"], "aac");
        assert_eq!(body["bitrate_kbps"], 160);
        assert_eq!(body["tunnel"]["state"], "online");

        let mut rebound = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(rebound, "GET /stream HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n").unwrap();
        let mut refused = String::new();
        rebound.read_to_string(&mut refused).unwrap();
        assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
        assert_eq!(broadcast.status().listeners, 0);
    }

    /// Encodes a second of the tap through the real ffmpeg. Needs ffmpeg on
    /// PATH (or FFMPEG_BIN), so it is not part of the normal run.
    ///
    ///     cargo test --bin defalt real_ffmpeg -- --ignored --nocapture
    #[test]
    #[ignore = "needs ffmpeg"]
    fn real_ffmpeg_encodes_the_tap() {
        let telemetry = Arc::new(Telemetry::default());
        let port = free_port();
        let mut settings = config(port, 64);
        if let Ok(bin) = std::env::var("FFMPEG_BIN") { settings.ffmpeg = bin.into(); }
        let broadcast = Broadcast::start(settings, Some(telemetry.clone())).unwrap();
        let mut client = get(port, "/stream");
        read_head_of(&mut client);
        assert!(wait_for(|| telemetry.broadcast.active()));
        let feeding = telemetry.clone();
        let feeder = std::thread::spawn(move || {
            for block in 0..100 {
                if let Some(writer) = feeding.broadcast.begin() {
                    for i in 0..480 {
                        let t = (block * 480 + i) as f32 / 48_000.0;
                        let v = (std::f32::consts::TAU * 440.0 * t).sin() * 0.3;
                        writer.put(i, v, v);
                    }
                    writer.commit(480, 48_000);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let mut got = vec![0u8; 4096];
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        client.read_exact(&mut got).unwrap();
        assert!(Codec::Aac.syncs(&got), "the stream does not start on an ADTS frame");
        feeder.join().unwrap();
        println!("{:?}", broadcast.status());
    }
}
