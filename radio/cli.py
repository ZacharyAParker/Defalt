"""python -m radio.cli <command>  -- setup, diagnostics, and dry runs.

    doctor    check that everything the station needs is actually working
    voices    list the Edge TTS voices you can put in a persona file
    tts-check   probe an OpenRouter speech model and its voices
    seed      import the Spotify bootstrap into the taste profile
    vault     rebuild every Obsidian note from the database
    break     write and render one talk break without going on air
    track     resolve, download and analyse a single track
    audit     check cached tracks for live takes and music videos
    purge     empty the audio cache
"""
from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from typing import Any

from . import config, db, library, llm, taste, tts, vault
from .segments import writers
from .sources import steam


def _ok(label: str, good: bool, detail: str = "", optional: bool = False) -> bool:
    """Print one check. Optional things are noted, not failed.

    Returns whether this should count against overall health.
    """
    mark = "ok  " if good else ("skip" if optional else "FAIL")
    # The hint only helps when something is actually wrong.
    suffix = f"  -- {detail}" if detail and not good else ""
    print(f"  [{mark}] {label}{suffix}")
    return good or optional


def cmd_doctor(_args: argparse.Namespace) -> int:
    print("checking the studio...\n")
    healthy = True

    healthy &= _ok("ffmpeg", shutil.which(config.FFMPEG) is not None,
                   "install it and put it on PATH")
    healthy &= _ok("ffprobe", shutil.which(config.FFPROBE) is not None,
                   "install it beside ffmpeg, or set FFPROBE_BIN")

    key = bool(config.env("OPENROUTER_API_KEY"))
    healthy &= _ok("OpenRouter key", key,
                   "hosts fall back to canned lines without it", optional=True)
    if llm.status()['configured']:
        # Free models rate-limit constantly and a cold one can time out on the
        # first call. One retry before reporting, and never fail the whole
        # check over it -- the station falls back to canned lines and stays up.
        reply = llm.complete("Reply with the single word: ready.",
                             "Say ready.", max_tokens=12, temperature=0)
        if not reply:
            reply = llm.complete("Reply with the single word: ready.",
                                 "Say ready.", max_tokens=12, temperature=0)
        healthy &= _ok(
            "Writing backend reachable", bool(reply),
            "no response twice -- free tiers throttle hard. The hosts will "
            "use canned lines until it recovers.", optional=True)

    voices = tts.list_voices()
    healthy &= _ok("Edge TTS", bool(voices), f"{len(voices)} voices available")

    personas = config.personas()
    healthy &= _ok("personas loaded", len(personas) >= 2,
                   ", ".join(personas) or "none found in config/personas")
    if voices and personas:
        installed = {v["name"] for v in voices}
        for pid, persona in personas.items():
            name = (persona.get("voice") or {}).get("name", "")
            _ok(f"voice for {pid}: {name}", name in installed,
                "" if name in installed else "will fall back to a default voice")

    healthy &= _ok("Steam configured", steam.configured(),
                   "patch notes and game ads need STEAM_API_KEY + STEAM_ID",
                   optional=True)
    if steam.configured():
        healthy &= _ok("Steam library visible", bool(steam.tracked_titles()),
                       "profile and game details must be public")

    count = (db.one("SELECT COUNT(*) AS n FROM tracks") or {"n": 0})["n"]
    healthy &= _ok(f"library ({count} tracks)", bool(count),
                   "run `python -m radio.cli seed`")

    print("\n" + ("ready to go on air." if healthy else
                  "fix the failures above before starting."))
    return 0 if healthy else 1


def cmd_voices(args: argparse.Namespace) -> int:
    voices = tts.list_voices()
    needle = (args.filter or "").lower()
    for voice in voices:
        line = f"{voice['name']:<40} {voice['gender']:<8} {voice['personality']}"
        if not needle or needle in line.lower():
            print(line)
    print(f"\n{len(voices)} voices. Put one in the `voice.name` field of a "
          f"persona file under config/personas/.")
    return 0


def cmd_tts_check(args: argparse.Namespace) -> int:
    """Probe an OpenRouter speech model: does it work, and which voices?

    Speech models live behind /audio/speech and are NOT listed in /models, so
    there is nothing to enumerate -- you have to name one and try it. This
    finds out what it accepts.
    """
    import httpx
    key = config.env("OPENROUTER_API_KEY")
    if not key:
        print("set OPENROUTER_API_KEY in .env first")
        return 1

    model = args.model or str(
        config.station.get("tts.openrouter.model",
                           "fish-audio/s2.1-pro-free:free"))
    print(f"probing {model} ...\n")

    headers = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}
    candidates = ["alloy", "ash", "ballad", "coral", "echo", "fable",
                  "onyx", "nova", "sage", "shimmer", "verse"]
    working: list[str] = []

    for name in candidates:
        try:
            response = httpx.post(
                tts.SPEECH_ENDPOINT, headers=headers, timeout=90,
                json={"model": model, "input": "One two three.",
                      "response_format": "mp3", "voice": name})
        except Exception as error:  # noqa: BLE001
            print(f"  {name:<10} could not reach OpenRouter: {error}")
            break
        if response.status_code == 200 and len(response.content) > 512:
            working.append(name)
            print(f"  {name:<10} ok  ({len(response.content)} bytes)")
        elif response.status_code == 401:
            print("  key rejected -- check OPENROUTER_API_KEY")
            return 1
        else:
            print(f"  {name:<10} rejected ({response.status_code})")

    print()
    if not working:
        print("no voice accepted. Either the model id is wrong, or it does not\n"
              "take a `voice` parameter at all -- try it without one.")
        return 1

    print(f"usable voices: {', '.join(working)}")
    if len(working) == 1:
        print(f"\nOnly one voice. Both hosts would sound identical, so put just\n"
              f"one persona on it -- set `engine: openrouter` in that persona's\n"
              f"voice block and leave the other on edge.")
    print(f"\nEdge remains the default: free, fast, {len(tts.list_voices())} voices.")
    return 0


def cmd_seed(args: argparse.Namespace) -> int:
    if not taste.SEED_FILE.exists():
        print(f"no seed file at {taste.SEED_FILE}\n"
              f"copy seed/spotify_seed.example.json to spotify_seed.json and "
              f"fill it in, or add tracks with `track` and let it learn.")
        return 1
    added = taste.import_seed(force=args.force)
    if not added and not args.force:
        print("already seeded -- pass --force to re-import")
    print(f"imported {added} tracks from the Spotify seed")
    summary = taste.summary(limit=8)
    print(f"library is now {summary['library_size']} tracks")
    for entry in summary["top_artists"][:8]:
        print(f"  {entry['score']:+6.2f}  {entry['artist']}")
    return 0


def cmd_vault(_args: argparse.Namespace) -> int:
    result = vault.rebuild_all()
    print(f"wrote {result['tracks']} track notes and {result['artists']} "
          f"artist notes into {config.VAULT_DIR}")
    return 0


def cmd_break(args: argparse.Namespace) -> int:
    kind = args.kind
    if kind not in writers.WRITERS:
        print(f"unknown segment. options: {', '.join(writers.WRITERS)}")
        return 1

    row = db.one("SELECT * FROM tracks ORDER BY RANDOM() LIMIT 1")
    track = dict(row) if row else {"title": "Some Song", "artist": "Somebody"}
    context = writers.build_context(kind, previous=None, next=track,
                                    speech_budget=16.0)
    lines = writers.compose(kind, context)

    print(f"\n--- {kind} ---")
    for line in lines:
        print(f"{line.host.upper():>5}: {line.text}")

    if args.render:
        personas = config.personas()
        total = 0.0
        for line in lines:
            voice = (personas.get(line.host) or {}).get("voice") or {}
            result = tts.say(line.text, voice)
            if result:
                total += result["duration"]
                print(f"       -> {result['duration']:.1f}s  {result['path']}")
        print(f"\ntotal speech: {total:.1f}s")
    return 0


def cmd_track(args: argparse.Namespace) -> int:
    artist, title = args.artist, args.title
    key = taste.add_track(title, artist, source="manual")
    row = db.one("SELECT * FROM tracks WHERE key=?", (key,))
    prepared = library.ensure(dict(row))
    if not prepared:
        print("could not resolve or download that one")
        return 1
    print(f"{prepared['artist']} - {prepared['title']}")
    print(f"  file      {prepared['file']}")
    print(f"  duration  {prepared['duration']:.1f}s")
    print(f"  intro     {db.intro_of(prepared, 12.0):.1f}s  "
          f"(hosts can talk this long over the top)")
    print(f"  outro     {prepared['outro_sec']:.1f}s")
    return 0


def cmd_audit(args: argparse.Namespace) -> int:
    """Check what each cached track was actually resolved from.

    Anything downloaded before the resolver learned to refuse live takes and
    music videos may have come from one. This re-reads the source metadata and
    reports; --fix drops the offenders so they are fetched again properly.
    """
    from yt_dlp import YoutubeDL

    rows = db.query("SELECT key,artist,title,video_id FROM tracks "
                    "WHERE video_id IS NOT NULL AND file IS NOT NULL")
    if not rows:
        print("nothing cached to audit")
        return 0

    print(f"checking {len(rows)} cached tracks...\n")
    options = {"quiet": True, "no_warnings": True, "skip_download": True,
               "socket_timeout": 15, "retries": 1, "extract_flat": True}
    suspect: list[tuple[str, str, list[str]]] = []

    with YoutubeDL(options) as ydl:
        for row in rows:
            try:
                info = ydl.extract_info(
                    f"https://www.youtube.com/watch?v={row['video_id']}",
                    download=False, process=False)
            except Exception:  # noqa: BLE001 - deleted, private, region locked
                continue
            entry = {
                "title": info.get("title") or "",
                "description": info.get("description") or "",
                "channel": info.get("channel") or "",
                "duration": info.get("duration") or 0,
            }
            kind = library.classify(entry, row["artist"], row["title"])
            tags = [t for t in ("live", "video", "tampered") if kind[t]]
            if tags:
                name = f"{row['artist']} - {row['title']}"
                suspect.append((row["key"], name, tags))
                print(f"  {name[:44]:46} {'+'.join(tags)}")
                print(f"      {entry['title'][:70]}")

    if not suspect:
        print("every cached track came from a clean source.")
        return 0

    print(f"\n{len(suspect)} of {len(rows)} came from a source the resolver "
          f"would now refuse.")
    if not args.fix:
        print("re-run with --fix to drop them and fetch them again.")
        return 0

    for key, name, _ in suspect:
        db.write("UPDATE tracks SET file=NULL, video_id=NULL, bpm=NULL, "
                 "beat_period=NULL WHERE key=?", (key,))
    print(f"dropped {len(suspect)}. They will be fetched again on next play.")
    return 0


def cmd_purge(_args: argparse.Namespace) -> int:
    library.purge_all()
    print("audio cache emptied")
    return 0


def cmd_reanalyse(args: argparse.Namespace) -> int:
    from . import analysis

    def progress(track: dict, ok: bool) -> None:
        mark = "ok  " if ok else "FAIL"
        print(f"{mark} {track.get('artist', '?')} - {track.get('title', '?')}", flush=True)

    counts = analysis.reanalyse(limit=args.limit, progress=progress)
    print(f"updated {counts.get('updated', 0)}, failed {counts.get('failed', 0)}, "
          f"still pending {counts.get('pending', 0)}")
    return 0


def main(argv: list[str] | None = None) -> int:
    # The Windows console defaults to cp1252, and half the library has an
    # accent or a CJK character in it. Without this, printing a track name is
    # enough to crash the command.
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, OSError):
            pass

    parser = argparse.ArgumentParser(prog="python -m radio.cli",
                                     description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    subparsers = parser.add_subparsers(dest="command", required=True)

    subparsers.add_parser("doctor").set_defaults(func=cmd_doctor)

    voices = subparsers.add_parser("voices")
    voices.add_argument("filter", nargs="?", help="substring to filter by")
    voices.set_defaults(func=cmd_voices)

    seed = subparsers.add_parser("seed")
    seed.add_argument("--force", action="store_true",
                      help="re-import even if the library is already seeded")
    seed.set_defaults(func=cmd_seed)

    subparsers.add_parser("vault").set_defaults(func=cmd_vault)
    tts_check = subparsers.add_parser("tts-check")
    tts_check.add_argument("model", nargs="?",
                           help="OpenRouter speech model id to probe")
    tts_check.set_defaults(func=cmd_tts_check)

    brk = subparsers.add_parser("break")
    brk.add_argument("kind", nargs="?", default="banter")
    brk.add_argument("--render", action="store_true", help="also run TTS")
    brk.set_defaults(func=cmd_break)

    track = subparsers.add_parser("track")
    track.add_argument("artist")
    track.add_argument("title")
    track.set_defaults(func=cmd_track)

    audit = subparsers.add_parser("audit")
    audit.add_argument("--fix", action="store_true",
                       help="drop tracks from bad sources so they refetch")
    audit.set_defaults(func=cmd_audit)

    subparsers.add_parser("purge").set_defaults(func=cmd_purge)

    reanalyse = subparsers.add_parser("reanalyse",
                                      help="refresh key, energy, phrase and similarity data")
    reanalyse.add_argument("--limit", type=int, default=None)
    reanalyse.set_defaults(func=cmd_reanalyse)

    args = parser.parse_args(argv)
    return int(args.func(args) or 0)


if __name__ == "__main__":
    sys.exit(main())
