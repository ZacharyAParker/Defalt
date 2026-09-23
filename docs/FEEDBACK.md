# Bug reports and suggestions

Both the console and the radio page have a way to report a bug or suggest
something, and both file it to the same place: a folder under `reports/` in
the project, listed in `reports/INBOX.md`. Nothing leaves the computer.

> **Working through the list:** open `reports/INBOX.md` for open items;
> each folder has `report.md`, `context.json` and `logs.txt` (and
> `screenshot.png` when one was taken). When an item is dealt with, close it
> with `python -m radio.feedback close <id> "what was done"`.

## Filing one

- **Console:** *Report a bug* in the footer, or **F8** anywhere. The window
  is photographed before the dialog opens, so the picture shows what you were
  looking at. Pick Bug or Suggestion, give it a title and a description, and
  optionally what you expected. *Attach logs* is on by default; *Include the
  current screen* attaches the picture.
- **Radio page:** *Report a bug* in the footer, next to Patches and Privacy.
  The page also sends the errors and warnings its own console printed
  (the last 200 lines), and what its audio elements were doing.

When the station is running, the console sends the report to it
(`POST /api/feedback`) along with the console's own context and log excerpt,
and the station adds its side. When the station is off or not answering, the
console writes the same folder itself (`src/reports.rs`), so a report never
depends on the thing that may be broken.

## Logs

Every line in both logs starts with an ISO 8601 local timestamp with its
offset (`2026-09-22T14:03:05.123-05:00`), which is what lets a report merge
them by time.

| File | Written by | What |
| --- | --- | --- |
| `logs/station.log` | `radio/feedback.py`, installed from `radio/__main__.py` | everything the station prints, `[out]` or `[err]` |
| `logs/console.log` | `src/logfile.rs` | the console's notices, radio notes, station health changes, audio and library errors (`[console]`), and every line the station child printed (`[station]`) |

Each rotates at 5 MB and keeps five old files (`station.log.1` is the newest
old one). `logs/` and `reports/` are ignored by git.

## The folder

```
reports/
  INBOX.md
  20260922-140305-skip-stopped-the-music/
    report.md
    context.json
    logs.txt
    screenshot.png      (console only, when included)
```

The id is the local time the report was filed plus a slug of its title; a
second report in the same second with the same title gets `-2`.

**INBOX.md** starts with a short header, then one line per report, oldest
first. The fields are separated by ` | ` (a `|` in a title is written as `/`):

```
- [ ] <id> | <YYYY-MM-DD HH:MM:SS> | <bug|suggestion> | open | <title> | <id>/report.md
- [x] <id> | <YYYY-MM-DD HH:MM:SS> | <bug|suggestion> | closed | <title> | <id>/report.md | closed <YYYY-MM-DD HH:MM>: <note>
```

**report.md** holds the title, id, when it was filed, type, status, which
client filed it, the app version, git commit and branch, the OS and Python
versions, the station's state, then the description, what was expected, and
a list of what is attached.

**context.json** has `id`, `filed`, `kind`, `client`, `environment`, and:

- `station` -- what the station said about itself: status note, clock,
  now playing, the next scheduled items, the queue, the last transcript
  lines, the vibe and private direction, recent director chat, the mix
  settings, model status, discovery, trends and ads. Anything it could not
  collect within three seconds is listed under `unavailable` instead.
  Written by the console alone, it says the station was not running.
- `browser` or `console` -- what the filing client could see. The console
  sends both decks (record, position, tempo, EQ, stems, errors), the audio
  device and underruns, the station's health and on-air reading, the radio
  schedule and queue as it last saw them, director chat, mix settings, the
  last lines the station printed, and its recent notices.

**logs.txt** is every line from every log file (rotated ones included) from
ten minutes before the report to one minute after, merged in time order,
at most 2000 lines (the newest are kept). Each line reads
`<timestamp> <source> <text>`, where the source is the log it came from
(`station`, `console`, `browser`). Lines the console copied from the station
are dropped when the station's own copy is there.

## Secrets

Nothing from `.env` is ever written. Every value in `.env` under a secret
name (API key, token, secret, password...) or long enough to be a key is
replaced with `[redacted]` wherever it appears, as are strings shaped like
API keys and tokens (`sk-...`, `Bearer ...`, `api_key=...`, `"token": "..."`,
private key blocks, and so on), in the description, the context and the
logs. Context fields with secret names have their whole value replaced. Track
keys (`artist|title`) are left alone.

## Reviewing

```
python -m radio.feedback list            # everything, open and closed
python -m radio.feedback list --open
python -m radio.feedback show <id>       # prints report.md and the other files' paths
python -m radio.feedback close <id> [note]
```

An id can be shortened to any unambiguous prefix. Closing marks the line in
INBOX.md and the status in report.md. `GET /api/feedback` lists the same
reports as JSON.
