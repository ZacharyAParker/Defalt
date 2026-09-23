# Listening away from home

Defalt can play the station on your phone in the car or at work: the exact
sound the console makes at home, with every transition (stem swaps,
spinbacks, rolls, echo, reverb) just as the console plays them, plus the
controls: skip, like, dislike, vibe, requests, Director chat and the queue.

## How it works

```
 phone (Safari / home-screen app)
   │  https://defalt.ccjitters.org
   ▼
 Cloudflare Access  ── email code; only you get in
   ▼
 Cloudflare Tunnel "defalt"  ── cloudflared, running on the PC at home
   ▼
 station (Python, 127.0.0.1:8090)  ── checks the Access token on every request
   │  /listen
   ▼
 console stream server (127.0.0.1:8091)  ── the console's own output, encoded
```

- **The console does the mixing.** The engine copies its final output (after
  the master and the limiter, which is exactly what your speakers get) into a
  buffer while someone is listening. ffmpeg encodes it to AAC at 160 kb/s and
  a small server on `127.0.0.1:8091` sends it to every listener. The browser
  mixer is not used remotely: iOS stops Web Audio when the phone locks, and
  it cannot do stems or play backwards anyway.
- **Nothing runs until somebody listens.** The encoder starts when the first
  listener connects and stops 30 seconds after the last one leaves. While
  anyone is listening, the PC is asked not to go to sleep.
- **The stream is a few seconds behind.** The page measures how far and
  holds the transcript and "now playing" back by the same amount, so the
  words line up with what you hear.
- **Every remote request is checked.** The station only answers the public
  name (`REMOTE_HOSTS`) when the request carries a valid Cloudflare Access
  token for your Access application. Anything else gets a 403. If
  `REMOTE_HOSTS` is set but the Access settings are not, remote requests are
  all refused. Loopback (the console, a browser on the PC) works as before.

## Setup (once, on the PC)

The tunnel `defalt` and the Access application ("Only Zach") already exist.
In `.env`:

```
REMOTE_HOSTS=defalt.ccjitters.org
CF_ACCESS_TEAM_DOMAIN=<team>.cloudflareaccess.com
CF_ACCESS_AUD=<the Access application's AUD tag>
REMOTE_TUNNEL=defalt
```

Optional: `CLOUDFLARED_BIN` (default: `cloudflared` on PATH, then
`C:\Program Files (x86)\cloudflared\cloudflared.exe`), `CLOUDFLARED_CONFIG`
(default: `%USERPROFILE%\.cloudflared\defalt.yml`), `BROADCAST_PORT` (8091),
`BROADCAST_CODEC` (`aac` or `mp3`), `BROADCAST_BITRATE` (160). ffmpeg must be
on PATH or named by `FFMPEG_BIN`.

## Starting it

Start the radio in the console as usual. **The tunnel starts with the radio**:
once the station answers, the console starts `cloudflared`, and the toolbar
shows **Remote: connecting**, then **Remote: online** (with the number of
people listening once someone is). When you stop the radio or close the
console, the tunnel is stopped first. If cloudflared dies it is restarted
with a growing delay. Its output goes to `logs/console.log`.

The tunnel can be switched off without touching `.env`: open the Board panel
in the page and untick **Run the tunnel while the radio is on**
(`remote.enabled` in `station.yaml`). The same panel has **Mute this PC's
speakers while someone streams** (`remote.mute_local`), so the house stays
quiet while you listen from elsewhere.

Running the station on its own (`python -m radio`, without the console) also
starts the tunnel once the server is listening, so the page and the controls
work remotely. There is nothing to stream without the console, though.

## On the phone

1. Open **https://defalt.ccjitters.org** in Safari.
2. Sign in with the code Cloudflare emails you.
3. Tap **Share → Add to Home Screen**.
4. Open Defalt from the home screen and tap **Start the station**.

Opened from the home screen (or on the public name at all), the page starts
in **Stream** mode. The button next to Start switches to **Mix here** (the
browser mixer) and back, and remembers your choice. The lock screen, Control
Center and **CarPlay** show what is playing in Now Playing, with the cover;
the next-track button skips.

## Troubleshooting

- **"The console isn't running at home"**: the console at home has to be
  running for there to be anything to hear. Start Defalt, then start the radio.
- **The page doesn't load at all**: the PC has to be on and the radio started
  (the tunnel only runs while it is). Check the toolbar says Remote: online.
- **403 "Cloudflare Access sign-in required"**: the request did not carry a
  valid Access token. Sign in again (sessions expire), and check that
  `CF_ACCESS_TEAM_DOMAIN` and `CF_ACCESS_AUD` match the Access application.
- **Silence, or the stream keeps reconnecting**: the page reconnects on its
  own with a growing delay. A weak signal in the car will do this; the stream
  picks up at live when it comes back.
- **No Remote indicator in the toolbar**: remote listening isn't set up. A
  `.env` setting is missing, `cloudflared` wasn't found, or the tunnel's
  config file isn't where it's expected. The page's Board panel shows the
  reason under Remote listening.
- **Remote: off** while the radio is on: the tunnel is switched off in the
  Board panel.

## Privacy

This is for your own listening. The station plays your library and
downloaded music to you; don't share the URL or add other people to the
Access policy. Everything stays on your PC; Cloudflare only carries the
traffic.
