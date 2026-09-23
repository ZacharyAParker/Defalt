"""The director: decides what airs next and puts it on the clock.

Runs background workers for preparation, discovery, and cache maintenance.

  feeder   -- keeps a small queue of fully downloaded, analysed tracks ready,
              so the playout path never waits on a network round trip.
  builder  -- extends the schedule ahead of the needle: picks segments off the
              clock, writes them, renders the voices, and back-times each
              break so the last word lands where it should.
  discovery -- refreshes public charts and verifies unfamiliar song suggestions.
  janitor   -- maintains the local audio cache.

The station clock only advances while something is actually listening. Close
the tab and the station holds its place instead of burning bandwidth.
"""
from __future__ import annotations

import json
import math
import random
import threading
import time
import uuid
from pathlib import Path
from typing import Any
from urllib.parse import quote

from . import analysis, config, db, discovery, library, taste, timeline, trends, tts, wishes, vibe
from .segments import writers
from .segments.base import Line

# How far ahead of the needle the schedule is kept, in seconds.
LOOKAHEAD = 150.0
# No heartbeat for this long and the station goes quiet.
# Must comfortably exceed browser background-tab timer throttling: a hidden
# tab is clamped to roughly one timer wake per minute, so anything under ~60s
# would take the station off the air the moment you switch tabs -- while the
# audio you already scheduled is still playing.
LISTENER_TIMEOUT = 100.0


def _log(*parts: Any) -> None:
    if config.DEBUG:
        print("[director]", *parts, flush=True)


def audio_present(track: dict[str, Any]) -> bool:
    return bool(track.get("file")) and Path(track["file"]).is_file()


def media_url(track: dict[str, Any]) -> str:
    """Where both players fetch a record's audio.

    Library files stay where the listener keeps them and are served by key;
    downloads live in the station's audio cache.
    """
    if track.get("source") == "local" and track.get("key"):
        return "/media/track/" + quote(str(track["key"]), safe="")
    return f"/media/audio/{Path(track['file']).name}"


class Clock:
    """Station time. Pauses when nobody is listening.

    Monotonic: a wall-clock correction (NTP, DST, a manual change) must never
    jump the schedule forward or back under the listener.
    """

    def __init__(self) -> None:
        self._elapsed = 0.0
        self._mark = 0.0
        self._running = False
        self._lock = threading.Lock()

    def now(self) -> float:
        with self._lock:
            if self._running:
                return self._elapsed + (time.monotonic() - self._mark)
            return self._elapsed

    def start(self) -> None:
        with self._lock:
            if not self._running:
                self._mark = time.monotonic()
                self._running = True

    def stop(self) -> None:
        with self._lock:
            if self._running:
                self._elapsed += time.monotonic() - self._mark
                self._running = False

    def jump(self, seconds: float) -> None:
        """Move station time forward. Used to skip into a transition."""
        with self._lock:
            self._elapsed += max(0.0, seconds)

    @property
    def running(self) -> bool:
        return self._running


class Station:
    def __init__(self) -> None:
        config.ensure_dirs()
        taste.import_seed()
        # In-memory queues do not survive a backend restart.
        db.write("UPDATE requests SET status='pending', note=NULL WHERE status='preparing'")
        db.write("UPDATE requests SET status='cancelled', note='Station restarted before this queue entry finished' "
                 "WHERE status IN ('queued','scheduled')")

        self.clock = Clock()
        self.schedule = timeline.Schedule()
        self.lock = threading.RLock()
        self.rng = random.Random()

        # The queue you can actually see and reorder. A plain list under the
        # station lock rather than a Queue, because a Queue cannot be
        # inspected, reordered or emptied, and all three are the point.
        self._lineup: list[dict[str, Any]] = []
        self._stop = threading.Event()
        self._last_heartbeat = 0.0
        self._songs_since_break = 0
        self._break_after = self._roll_break_gap()
        self._recent_keys: list[str] = []
        self._last_track: dict[str, Any] | None = None
        self._next_is_request = False
        self.status_note = "warming up"
        self._active_wish: dict[str, Any] | None = None
        # Bumped whenever the clock moves discontinuously. The browser watches
        # it and re-syncs, because otherwise it would keep playing to the old
        # timeline and never notice the skip.
        self._epoch = 0
        # The sign-on runs once per time the station comes up, ahead of the
        # rotation. `db.last_aired` says whether this is a first night or a
        # return, which is the only difference the hosts are told about.
        self._signed_on = False
        self._transcript: dict[str, dict[str, Any]] = {}
        self._skipped_speech: set[str] = set()
        self._pending_skip: str | None = None
        self._building_entry: dict[str, Any] | None = None
        self._threads: list[threading.Thread] = []

    # -- lifecycle -------------------------------------------------------
    def start(self) -> None:
        for target in (self._feeder_loop, self._builder_loop, self._janitor_loop, self._discovery_loop):
            thread = threading.Thread(target=target, daemon=True,
                                      name=target.__name__)
            thread.start()
            self._threads.append(thread)

    def shutdown(self) -> None:
        self._stop.set()
        self.clock.stop()
        if config.station.get("learning.write_session_notes", True):
            try:
                from . import vault
                vault.write_session_note()
            except Exception as error:  # noqa: BLE001
                _log("session note failed", error)
        if config.station.get("cache.purge_on_exit", False):
            library.purge_all()

    def heartbeat(self) -> None:
        self._last_heartbeat = time.monotonic()
        self.clock.start()

    def start_decks(self, tracks: list[dict[str, Any]], session: str) -> dict[str, Any]:
        """Adopt the console's opening pair before allowing the builder to run.

        Retried HTTP requests must return the same lineup and item IDs; resetting
        the clock again would make the console continually reload its own decks.
        """
        import math
        if not isinstance(session, str) or not session or len(session) > 100:
            raise ValueError("need a startup session")
        if not isinstance(tracks, list) or len(tracks) > 2:
            raise ValueError("expected zero, one or two loaded decks")
        with self.lock:
            if getattr(self, "_deck_start_token", None) == session:
                self.heartbeat()
                return self.snapshot()
            prepared = []
            used = set()
            for selection in tracks:
                if not isinstance(selection, dict):
                    raise ValueError("invalid deck selection")
                deck = selection.get("deck")
                if type(deck) is not int or deck not in (0, 1) or deck in used:
                    raise ValueError("each selection needs its own deck (0 or 1)")
                used.add(deck)
                key = selection.get("key")
                if not isinstance(key, str) or not key:
                    raise ValueError("missing track key")
                row = db.one("SELECT * FROM tracks WHERE key=?", (key,))
                if row is None:
                    raise ValueError(f"unknown opening track: {key}")
                track = dict(row)
                duration = float(track.get("duration") or 0)
                offset = float(selection.get("offset", 0))
                if (not math.isfinite(duration) or duration <= 0
                        or not math.isfinite(offset) or not 0 <= offset < duration
                        or not track.get("file") or not Path(track["file"]).is_file()):
                    raise ValueError(f"opening track is not playable: {key}")
                track["selection_origin"] = {"by": "listener", "method": "preloaded_deck"}
                prepared.append((deck, track, offset))

            if prepared:
                schedule = timeline.Schedule(self.clock.now() + 0.25)
                for deck, track, offset in prepared:
                    item = schedule.add_music(
                        media_url(track), track, offset=offset, entry_locked=True,
                        earliest_start=self.clock.now() + 0.25)
                    item.meta["deck"] = deck
                schedule.seal()
                keys = {track["key"] for _, track, _ in prepared}
                for entry in self._lineup:
                    if entry["track"].get("key") in keys:
                        self._cancel_entry(entry, "Started from a preloaded deck")
                self._lineup = [e for e in self._lineup if e["track"].get("key") not in keys]
                self._finish_requests(self.clock.now())
                for old in self.schedule.music_items():
                    self._cancel_entry({"request_id": (old.meta.get("selection_origin") or {}).get("request_id")},
                                       "Replaced by preloaded decks")
                self.schedule = schedule
                self._pending_skip = None
                self._last_track = prepared[-1][1]
                self._recent_keys = (self._recent_keys + [t["key"] for _, t, _ in prepared])[-40:]
                self._songs_since_break += len(prepared)
                self._epoch += 1
                self.status_note = "opening decks ready"
            self._deck_start_token = session
            self.heartbeat()
            return self.snapshot()

    def _listening(self) -> bool:
        return (time.monotonic() - self._last_heartbeat) < LISTENER_TIMEOUT

    # -- clock rules -----------------------------------------------------
    def _roll_break_gap(self) -> int:
        span = config.station.get("clock.songs_per_break", [2, 4]) or [2, 4]
        try:
            low, high = int(span[0]), int(span[1])
        except (TypeError, ValueError, IndexError):
            low, high = 2, 4
        return self.rng.randint(min(low, high), max(low, high))

    def _cooldown_ok(self, kind: str) -> bool:
        minutes = (config.station.get("clock.cooldown_minutes", {}) or {}).get(kind)
        if not minutes:
            return True
        last = db.last_aired(kind)
        return not last or (time.time() - last) >= float(minutes) * 60

    def _choose_segment(self) -> str:
        # The station has to say hello before it says anything else. Not a
        # station ID: an ID reminds you which station this is, and nobody has
        # tuned in by accident.
        if not self._signed_on:
            self._signed_on = True
            if config.station.get("clock.sign_on", True):
                return "sign_on"

        # Anything you asked for outranks the clock. Checked here rather than
        # in the weights so a request cannot be lost to a cooldown.
        wish = wishes.next_segment()
        if wish:
            payload = json.loads(wish["payload"] or "{}")
            self._active_wish = wish
            return str(payload.get("segment") or "banter")

        topic = wishes.next_topic()
        if topic:
            self._active_wish = topic
            return "article" if topic['kind'] == 'article' else "topic"

        # Top of the hour is news, if it is due and enabled.
        if config.station.get("clock.top_of_hour_news", True):
            window = float(config.station.get("clock.top_of_hour_window", 4) or 4)
            if time.localtime().tm_min < window and self._cooldown_ok("news"):
                return "news"

        weights = config.station.get("clock.segment_weights", {}) or {}
        pool = [
            (kind, float(weight))
            for kind, weight in weights.items()
            if kind in writers.WRITERS and float(weight or 0) > 0
            and self._cooldown_ok(kind)
            and (kind != "game_ad" or config.games.get("ads.enabled", True))
        ]
        if not pool:
            return "banter"
        kinds = [k for k, _ in pool]
        return self.rng.choices(kinds, weights=[w for _, w in pool], k=1)[0]

    # -- feeder ----------------------------------------------------------
    def _discovery_loop(self):
        while not self._stop.is_set():
            if self._listening():
                try:
                    self.trend_status = trends.refresh()
                    self.discovery_status = discovery.refresh(cancelled=lambda: self._stop.is_set() or not self._listening())
                except Exception as error:
                    self.discovery_status = {'state': 'unavailable', 'added': 0}
                    _log('discovery unavailable', error)
            self._stop.wait(30)

    def _next_candidate(self) -> tuple[dict[str, Any] | None, bool]:
        """Next track to prepare, and whether it came from a request."""
        pending = db.one(
            "SELECT * FROM requests WHERE status='pending' ORDER BY ts LIMIT 1")
        if pending:
            key = pending["track_key"]
            row = db.one("SELECT * FROM tracks WHERE key=?", (key,)) if key else None
            if row:
                db.write("UPDATE requests SET status='preparing', note=NULL WHERE id=? AND status='pending'",
                         (pending["id"],))
                return ({**dict(row), "_request_id": pending["id"]}, True)
            db.write("UPDATE requests SET status='failed', note=? WHERE id=?",
                     ("could not resolve", pending["id"]))

        # Exclude what has just played AND what is already waiting. Without
        # the second half the selector happily queues the same record twice,
        # because as far as it knows nothing has played it yet.
        # Played records use persistent cooldowns in taste.pick_next. Only
        # currently reserved records are absolute exclusions, so a small
        # library can eventually return to its longest-rested recording.
        exclude = set()
        with self.lock:
            exclude.update(e["track"].get("key") for e in self._lineup)
            exclude.update(i.meta.get("key") for i in self.schedule.music_items())
            # Append order is actual playback order: recent records, then the
            # scheduled decks, then the editable queue. Its tail is the pair
            # the next automatic choice must follow, not just 'now playing'.
            context_keys = list(self._recent_keys)
            context_keys.extend(i.meta.get("key") for i in self.schedule.music_items())
            queued = [dict(entry["track"]) for entry in self._lineup]
            building = getattr(self, "_building_entry", None)
            if building:
                exclude.add(building["track"].get("key"))
                queued.insert(0, dict(building["track"]))
            # Songs since the last host break by the time this pick airs, so
            # the energy arc can build across a run of music.
            since_break = getattr(self, "_songs_since_break", 0) + len(queued)
        exclude.discard(None)
        context = []
        # Recent history and the live schedule overlap. Keep the latest
        # occurrence so a repeated record remains at its chronological place.
        for key in reversed(dict.fromkeys(reversed(context_keys))):
            row = db.one("SELECT * FROM tracks WHERE key=?", (key,)) if key else None
            if row:
                context.append(dict(row))
        context.extend(queued)
        context = context[-int(max(2, min(30, config.station.get(
            "selection.compatibility.history_size", 10)))):]
        return (taste.pick_next(exclude, previous=context[-1] if context else None,
                                history=context, break_position=since_break), False)

    def _lineup_target(self) -> int:
        return max(1, int(config.station.get("selection.prefetch_depth", 5) or 5))

    def _feeder_loop(self) -> None:
        while not self._stop.is_set():
            try:
                worked = self._feed_once()
            except Exception as error:
                # One failed lookup must not kill the only feeder thread.
                _log("feeder failed", repr(error))
                self.status_note = "song preparation failed; retrying the queue"
                worked = False
            self._stop.wait(0.5 if worked else 1.0)

    def _feed_once(self) -> bool:
        if not self._listening():
            return False
        with self.lock:
            automatic = sum(e.get("source") == "auto" for e in self._lineup)
        # A full automatic buffer must never starve a listener's request.
        if automatic >= self._lineup_target() and not db.one(
                "SELECT id FROM requests WHERE status='pending' LIMIT 1"):
            return False
        vibe_revision = vibe.selection_revision()
        track, was_request = self._next_candidate()
        if not track:
            self.status_note = "no playable tracks in the library"
            return False
        request_id = track.get("_request_id") if was_request else None
        try:
            prepared = library.ensure(track)
            if not prepared or not prepared.get("duration"):
                raise RuntimeError("No usable audio found. Check the artist/title and retry.")
        except Exception as error:
            message = str(error)[:400] or "Song preparation failed. Please retry."
            if request_id:
                db.write("UPDATE requests SET status='failed', note=? WHERE id=? AND status='preparing'",
                         (message, request_id))
            self.status_note = f"Could not prepare {track.get('title')}: {message}"
            _log(self.status_note)
            return False
        try:
            # Records prepared before the energy/similarity descriptors (or a
            # better key reader) existed are brought up to date as they come
            # round, one decode each, here on the feeder thread.
            prepared = analysis.ensure_features(prepared)
        except Exception as error:  # noqa: BLE001 - descriptors are optional
            _log("feature analysis failed", repr(error))
        if track.get("selection"):
            prepared["selection"] = track["selection"]
        with self.lock:
            if not was_request and vibe_revision != vibe.selection_revision():
                return True  # A newer brief superseded this automatic pick.
            if not was_request:
                reserved = [e["track"] for e in self._lineup]
                reserved.extend(i.meta for i in self.schedule.music_items())
                building = getattr(self, "_building_entry", None)
                if building:
                    reserved.append(building["track"])
                if any(taste.recording_ids(prepared) & taste.recording_ids(t) for t in reserved):
                    return False  # A deck/request changed while this file was preparing.
            if request_id:
                state = db.one("SELECT status FROM requests WHERE id=?", (request_id,))
                if not state or state["status"] != "preparing":
                    return True  # Cancelled while audio was downloading.
                db.write("UPDATE requests SET status='queued', note=NULL WHERE id=?", (request_id,))
                prepared = {**prepared, "_request_id": request_id}
            placement = str(config.station.get("requests.placement", "after_break") or "after_break")
            self._enqueue(prepared, "request" if was_request else "auto",
                          front=was_request and placement != "queue")
        return True

    # -- the lineup ------------------------------------------------------
    def _enqueue(self, track: dict[str, Any], source: str,
                 front: bool = False) -> str:
        """Put a prepared track into the queue. Returns its handle."""
        track = dict(track)
        request_id = track.pop("_request_id", None) if source == "request" else None
        track.pop("_request_id", None)
        track["selection_origin"] = {"by": "listener" if source == "request" else "director",
                                     "method": ("request" if request_id else "manual_queue") if source == "request" else "automatic",
                                     "request_id": request_id}
        entry = {
            "id": uuid.uuid4().hex[:10],
            "track": track,
            "source": source,
            "request_id": request_id,
            "added_at": time.time(),
        }
        with self.lock:
            if front:
                self._lineup.insert(0, entry)
            else:
                self._lineup.append(entry)
        return entry["id"]

    def _take_next(self) -> dict[str, Any] | None:
        with self.lock:
            if not self._lineup:
                return None
            return self._lineup.pop(0)

    def lineup(self) -> list[dict[str, Any]]:
        """What is queued but not yet placed on the clock."""
        with self.lock:
            entries = list(self._lineup)
        return [
            {
                "id": entry["id"],
                "key": entry["track"].get("key"),
                "artist": entry["track"].get("artist"),
                "title": entry["track"].get("title"),
                "duration": entry["track"].get("duration"),
                "bpm": entry["track"].get("bpm"),
                "camelot": entry["track"].get("camelot"),
                "source": entry["source"],
                "selection_origin": entry["track"].get("selection_origin", {"by": "unknown"}),
                "selection": entry["track"].get("selection"),
            }
            for entry in entries
        ]

    def move(self, entry_id: str, where: str) -> bool:
        """Reorder the queue. `where` is next, up, down, or last."""
        with self.lock:
            index = next((i for i, e in enumerate(self._lineup)
                          if e["id"] == entry_id), None)
            if index is None:
                return False
            entry = self._lineup.pop(index)
            if where == "next":
                self._lineup.insert(0, entry)
            elif where == "up":
                self._lineup.insert(max(0, index - 1), entry)
            elif where == "down":
                self._lineup.insert(min(len(self._lineup), index + 1), entry)
            else:
                self._lineup.append(entry)
            return True

    def remove(self, entry_id: str) -> bool:
        with self.lock:
            for index, entry in enumerate(self._lineup):
                if entry["id"] == entry_id:
                    self._lineup.pop(index)
                    self._cancel_entry(entry, "Removed from queue")
                    return True
        return False

    def _cancel_entry(self, entry, note):
        request_id = entry.get("request_id")
        if request_id:
            db.write("UPDATE requests SET status='cancelled', note=? WHERE id=? AND status IN ('queued','scheduled')",
                     (note, request_id))

    def _finish_requests(self, now):
        self._credit_completions(now)
        finished = getattr(self, "_finished_request_ids", set())
        for item in self.schedule.music_items():
            request_id = (item.meta.get("selection_origin") or {}).get("request_id")
            if request_id and request_id not in finished and item.start_at <= now:
                if now < item.end_at:
                    db.write("UPDATE requests SET status='aired', note=NULL WHERE id=? AND status='scheduled'", (request_id,))
                else:
                    self._cancel_entry({"request_id": request_id}, "Passed without a playback report")
                finished.add(request_id)
        self._finished_request_ids = finished

    def _credit_completions(self, now):
        """A record that reached its planned end without a skip was heard out.

        Players only report what they display, and the console reports only
        starts, so completion is inferred here, once per airing. The clock
        only runs while someone is listening, so reaching the end means it
        was actually heard. Client 'played' reports share the same ledger.
        """
        credited = getattr(self, "_credited_items", {})
        skipped = getattr(self, "_skipped_items", set())
        for item in self.schedule.music_items():
            key, ident = item.meta.get("key"), getattr(item, "id", None)
            if (not key or not ident or ident in credited or ident in skipped
                    or item.end_at > now or item.start_at > now):
                continue
            credited[ident] = key
            source_end = item.offset + timeline.playback.source_at(
                item.meta.get("rate_curve"), item.duration, item.meta.get("playback_rate", 1.0))
            try:
                taste.record("played", key, item.meta.get("artist") or "",
                             position=source_end, duration=source_end)
            except Exception as error:  # noqa: BLE001 - learning must not stop playout
                _log("completion not recorded", error)
        self._credited_items = credited

    def _prune_memory(self):
        """Keep per-item ledgers to what the live schedule can still refer to."""
        live = {i.id for i in self.schedule.items}
        live_requests = {(i.meta.get("selection_origin") or {}).get("request_id")
                         for i in self.schedule.music_items()}
        self._reported_plays = {t for t in getattr(self, "_reported_plays", set()) if t in live}
        self._skipped_speech = {t for t in getattr(self, "_skipped_speech", set()) if t in live}
        self._skipped_items = {t for t in getattr(self, "_skipped_items", set()) if t in live}
        self._credited_items = {k: v for k, v in getattr(self, "_credited_items", {}).items() if k in live}
        self._finished_request_ids = {r for r in getattr(self, "_finished_request_ids", set())
                                      if r in live_requests}

    def refresh_vibe(self):
        """Replace unplanned automatic picks; decks and explicit requests stay."""
        with self.lock:
            self._lineup = [entry for entry in self._lineup if entry.get("source") != "auto"]

    def clear_lineup(self, keep_requests: bool = False) -> int:
        """Empty the queue. The feeder refills the automatic side."""
        with self.lock:
            before = len(self._lineup)
            if keep_requests:
                self._lineup = [e for e in self._lineup
                                if e["source"] == "request"]
            else:
                for entry in self._lineup:
                    self._cancel_entry(entry, "Queue cleared")
                self._lineup = []
            return before - len(self._lineup)

    def enqueue_key(self, track_key: str, front: bool = False) -> bool:
        """Queue a track that is already prepared, by key."""
        row = db.one("SELECT * FROM tracks WHERE key=?", (track_key,))
        if not row:
            return False
        track = dict(row)
        if not track.get("file") or not track.get("duration"):
            return False
        self._enqueue(track, "request", front=front)
        return True

    def drop_scheduled(self, item_id: str) -> bool:
        """Remove a record that is placed but has not started yet.

        Everything after it goes too, because the times and transitions of
        the following items were computed against this one. The builder
        refills from the queue within a second or two, so in practice you
        lose the thing you asked to lose and nothing else.
        """
        with self.lock:
            now = self.clock.now()
            target = next((i for i in self.schedule.items
                           if i.id == item_id and i.kind == "music"), None)
            if target is None or target.start_at <= now:
                return False

            cut = target.start_at
            for item in self.schedule.music_items():
                if item.start_at >= cut:
                    self._cancel_entry({"request_id": (item.meta.get("selection_origin") or {}).get("request_id")},
                                       "Removed from the planned schedule")
            # Also gives the record before it back the ending its transition
            # into the removed one had cut short.
            self.schedule.drop_from(cut, now)
            self.schedule.seal()
        return True

    # -- builder ---------------------------------------------------------
    def _needs_extension(self, now: float) -> bool:
        """Reserve the next handoff even when the current record is very long."""
        with self.lock:
            ahead = int(config.station.get("transitions.prepare_tracks_ahead", 1))
            ahead = max(1, min(3, ahead))
            future = sum(i.start_at > now for i in self.schedule.music_items())
            return future < ahead or self.schedule.end_at - now <= LOOKAHEAD

    def _builder_loop(self) -> None:
        while not self._stop.is_set():
            with self.lock:
                self._service_deferred_skip()
            if hasattr(self, "ads"):
                with self.lock:
                    self.ads.tick()
            if not self._listening():
                self.clock.stop()
                self._stop.wait(1.0)
                continue

            now = self.clock.now()
            if not self._needs_extension(now):
                self._stop.wait(1.0)
                continue

            try:
                self._extend()
            except Exception as error:  # noqa: BLE001 - never kill the builder
                _log("extend failed", repr(error))
                self._stop.wait(2.0)

    def _extend(self) -> None:
        with self.lock:
            entry = self._take_next()
            self._building_entry = entry
        if entry is None:
            self.status_note = "waiting on the next track"
            self._stop.wait(1.0)
            return
        track = entry["track"]

        with self.lock:
            planned_schedule = self.schedule
            planned_tail = self.schedule._last_music
            was_request = entry.get("source") == "request"
            do_break = (self._songs_since_break >= self._break_after
                        or not self._signed_on)
            chat = getattr(self, 'director_chat', None)
            if chat and chat.quiet_until > time.time() and self._signed_on and not (wishes.next_segment() or wishes.next_topic()):
                do_break = False
            lines: list[Line] = []
            kind = ""
            if do_break:
                kind = self._choose_segment()
                if (kind != "sign_on" and was_request and not self._active_wish
                        and config.station.get("requests.acknowledge_on_air", True)):
                    kind = "track_intro"
            previous = self._last_track
            active_wish = self._active_wish

        # Writing and voice synthesis can take many seconds. Schedule polling
        # and Skip must keep responding while that work happens.
        try:
            if do_break:
                context = writers.build_context(
                    kind,
                    previous=previous,
                    next=track,
                    was_request=was_request,
                    returning=bool(db.last_aired("sign_on")),
                    speech_budget=self._speech_budget(kind, track),
                    recent_host_lines=list(getattr(self, "_recent_host_lines", []))[-16:],
                )
                if active_wish and kind == "topic":
                    from .sources import rss
                    subject = active_wish["subject"]
                    context["topic"] = subject
                    context["topic_stories"] = rss.search(subject, limit=3)
                if active_wish and kind == "article":
                    context["article"] = json.loads(active_wish['payload'])
                if active_wish and kind == 'listener_message':
                    context['listener_message'] = json.loads(active_wish['payload']).get('message', '')
                lines = writers.compose(kind, context)

            voices = self._render(lines)
            placement = self._roll_placement() if voices else "none"
        except Exception:
            with self.lock:
                self._lineup.insert(0, entry)
                self._building_entry = None
            raise

        with self.lock:
            self._building_entry = None
            if self.schedule is not planned_schedule or self.schedule._last_music is not planned_tail:
                # A deck restart or queue removal invalidated this pair while
                # speech was rendering. Rebuild against the new pair.
                if not any(i.meta.get("key") == track["key"] for i in self.schedule.music_items()):
                    self._lineup.insert(0, entry)
                return
            if active_wish:
                wish_state = db.one('SELECT status FROM wishes WHERE id=?', (active_wish['id'],))
                if not wish_state or wish_state['status'] not in ('pending', 'active'):
                    voices, lines, placement = [], [], 'none'
            if not self._place(track, voices, placement):
                self._audio_missing(entry)
                if kind == "sign_on":
                    self._signed_on = False  # say hello with the next record instead
                return
            for item in self.schedule.items:
                if item.kind == "voice" and item.meta.get("segment") is None:
                    item.meta["segment"] = {"game_ad": "Ad break", "news": "News", "article": "News",
                                            "patch_notes": "Game updates", "sign_on": "Station welcome"}.get(kind, "Host break")
            self._recent_host_lines = (getattr(self, "_recent_host_lines", [])
                                       + [line.text for line in lines])[-32:]

            if do_break:
                if kind != "game_ad" or voices:
                    db.mark_aired(kind)
                if self._active_wish:
                    db.write("UPDATE wishes SET status='done' WHERE id=? AND status IN ('pending','active')",
                             (self._active_wish['id'],))
                    self._active_wish = None
                self._songs_since_break = 0
                self._break_after = self._roll_break_gap()
            self._songs_since_break += 1

            self._last_track = track
            self._recent_keys.append(track["key"])
            self._recent_keys = self._recent_keys[-40:]
            if entry.get("request_id"):
                db.write("UPDATE requests SET status='scheduled', note=NULL WHERE id=? AND status='queued'",
                         (entry["request_id"],))
            self.status_note = "on air"
            if hasattr(self, "ads"):
                self.ads.tick()
            pending = getattr(self, "_pending_skip", None)
            if pending:
                self._pending_skip = None
                now = self.clock.now()
                if any(i.id == pending and i.start_at <= now < i.end_at
                       for i in self.schedule.music_items()):
                    self._skip_locked(record=False)

    def _audio_missing(self, entry: dict[str, Any]) -> None:
        """A queued record lost its audio. Requests go back to be prepared
        again; automatic picks are simply dropped and the feeder refills."""
        request_id = entry.get("request_id")
        if request_id:
            db.write("UPDATE requests SET status='pending', note=? WHERE id=? AND status='queued'",
                     ("Audio was removed from the cache before it aired; preparing it again", request_id))
        self.status_note = f"re-preparing {entry['track'].get('title') or 'a song'}; its audio went missing"
        _log(self.status_note)

    def _speech_budget(self, kind: str, track: dict[str, Any]) -> float:
        """Roughly how long this break has to play with."""
        if kind == 'article':
            return 45.0
        if kind in ("news", "patch_notes", "game_ad"):
            return 28.0
        intro = db.intro_of(
            track, config.station.get("talk_placement.assumed_intro", 12.0))
        return max(6.0, min(intro, 20.0))

    def _roll_placement(self) -> str:
        weights = config.station.get("talk_placement.weights", {}) or {}
        pool = [(k, float(v or 0)) for k, v in weights.items() if float(v or 0) > 0]
        if not pool:
            return "bridge"
        return self.rng.choices([k for k, _ in pool],
                                weights=[w for _, w in pool], k=1)[0]

    def _render(self, lines: list[Line]) -> list[timeline.VoiceLine]:
        personas = config.personas()
        voices = [dict((personas.get(line.host) or {}).get("voice") or {}) for line in lines]
        # Render the break's lines together when the voice layer can; a line
        # the break cannot do without drops the whole break rather than air
        # half a conversation.
        say_many = getattr(tts, "say_many", None)
        results = (say_many([(line.text, voice) for line, voice in zip(lines, voices)]) if say_many
                   else [tts.say(line.text, voice) for line, voice in zip(lines, voices)])
        if getattr(tts, "required_failed", lambda *_: False)(lines, results):
            return []
        rendered: list[timeline.VoiceLine] = []
        for line, voice, result in zip(lines, voices, results):
            if not result:
                continue
            rendered.append(timeline.VoiceLine(
                url=f"/media/voice/{Path(result['path']).name}",
                duration=result["duration"],
                host=line.host,
                text=line.text,
                gain=float(voice.get("gain", 1.0) or 1.0),
                reference=line.reference,
            ))
        return rendered

    def _place(self, track: dict[str, Any], voices: list[timeline.VoiceLine],
               placement: str) -> bool:
        """Put the music and the break on the clock, correctly back-timed."""
        cfg = config.station
        if not audio_present(track):
            # The cache janitor can evict a queued record's file while it
            # waits in the lineup. Never schedule audio that is not there.
            return False
        url = media_url(track)
        previous = self.schedule._last_music  # noqa: SLF001 - same module family

        # Lay the lines out relative to zero so we know how long the break runs.
        laid = timeline.lay_out_lines(voices, 0.0, self.rng) if voices else []
        speech_start_rel, speech_end_rel = timeline.speech_span(laid)
        speech_length = speech_end_rel - speech_start_rel

        # A dry break needs its airtime reserved before the music is placed.
        if placement == "dry" and previous is not None and speech_length > 0:
            self.schedule.cursor = previous.end_at + 0.6 + speech_length + 0.6

        music = self.schedule.add_music(
            url, track, dry_before=(placement == "dry" and previous is not None),
            earliest_start=self.clock.now() + 2.0)

        if not laid:
            self.schedule.seal()
            return True

        intro = db.intro_of(
            track, cfg.get("talk_placement.assumed_intro", 12.0) or 12.0)
        from . import playback
        intro = playback.wall_at(music.meta.get("rate_curve"), max(0.0, intro - music.offset),
                                 music.meta.get("playback_rate", 1.0))
        safety = float(cfg.get("talk_placement.post_safety_margin", 0.6) or 0.0)

        # The rolled style may not fit this particular record. A three-second
        # intro cannot hold a nine-second break, so take the nearest style
        # that can rather than talking over the singer.
        if placement != "dry":
            placement = timeline.choose_placement(
                placement, intro=intro, safety=safety,
                speech_length=speech_length, music_start=music.start_at,
                previous_end=previous.end_at if previous else None,
                previous_start=previous.start_at if previous else None)

        start = timeline.break_start(
            "dry" if previous is None else placement,
            music_start=music.start_at,
            intro=intro,
            safety=safety,
            speech_length=speech_length,
            previous_end=previous.end_at if previous else None,
            previous_start=previous.start_at if previous else None,
            allow_backtime=bool(cfg.get(
                "talk_placement.allow_backtime_into_outro", True)),
            max_backtime=float(cfg.get("talk_placement.max_backtime", 20.0) or 0.0),
        )
        start = max(start, self.clock.now() + 1.0)

        # A manually inserted ad may already occupy this part of the clock.
        # Keep new host breaks after it without moving either music deck.
        for voice in sorted((i for i in self.schedule.items if i.kind == "voice"), key=lambda i: i.start_at):
            if voice.start_at < start + speech_length + 0.6 and voice.end_at + 0.6 > start:
                start = voice.end_at + 0.6

        for line, offset in laid:
            self.schedule.add_voice(
                line.url, start + (offset - speech_start_rel), line.duration,
                gain=line.gain,
                meta={"host": line.host, "text": line.text,
                      **({"reference": line.reference} if line.reference else {})},
            )

        window_end = start + speech_length
        if placement != "dry":
            self.schedule.duck_all_overlapping(start, window_end)
        self.schedule.seal()
        return True

    # -- housekeeping ----------------------------------------------------
    def _janitor_loop(self) -> None:
        while not self._stop.is_set():
            self._stop.wait(90)
            if self._stop.is_set():
                break
            try:
                keep_audio, keep_voice = self._janitor_protected()
                library.evict(protect=keep_audio)
                tts.evict(keep=keep_voice)
                from . import housekeeping
                housekeeping.sweep(keep_audio)
            except Exception as error:  # noqa: BLE001
                _log("janitor failed", error)

    def _janitor_protected(self) -> tuple[set[str], set[str]]:
        """Every audio and voice file the station still means to play.

        Not just what is on the clock: the queue and the entry the builder
        is placing right now hold prepared files too, and deleting one of
        those leaves a gap exactly where a song was promised.
        """
        with self.lock:
            self._finish_requests(self.clock.now())
            self.schedule.trim_before(self.clock.now())
            urls = {i.url for i in self.schedule.items}
            tracks = [i.meta for i in self.schedule.music_items()]
            tracks.extend(entry["track"] for entry in self._lineup)
            building = getattr(self, "_building_entry", None)
            if building:
                tracks.append(building["track"])
        keep_audio = {str(library.AUDIO_DIR / Path(u).name) for u in urls if "/audio/" in u}
        keep_audio.update(str(t["file"]) for t in tracks if t.get("file"))
        keep_voice = {str(tts.VOICE_DIR / Path(u).name) for u in urls if "/voice/" in u}
        return keep_audio, keep_voice

    # -- public surface --------------------------------------------------
    def snapshot(self) -> dict[str, Any]:
        with self.lock:
            self._service_deferred_skip()
            now = self.clock.now()
            self._finish_requests(now)
            if hasattr(self, "ads"):
                self.ads.tick()
            self.transcript()
            self.schedule.trim_before(now)
            self._prune_memory()
            items = self.schedule.as_dict()
            # Contract: `now` is read last, after the work above, so a slow
            # snapshot never hands the players a stale clock.
            now = self.clock.now()
        return {
            "now": round(now, 4),
            "items": items,
            "status": self.status_note,
            "discovery": getattr(self, 'discovery_status', {'state': 'waiting', 'added': 0}),
            "trends": getattr(self, 'trend_status', {'state': 'waiting'}),
            "running": self.clock.running,
            "queued": len(self._lineup),
            "epoch": self._epoch,
            "ad": self.ads.public() if hasattr(self, "ads") else {"enabled": bool(config.games.get("ads.enabled", True)), "busy": False},
        }

    def transcript(self) -> list[dict[str, Any]]:
        """Recent host lines, retained after audio leaves the short timeline."""
        with self.lock:
            now = self.clock.now()
            history = getattr(self, "_transcript", {})
            skipped = getattr(self, "_skipped_speech", set())
            for item in self.schedule.items:
                if (item.kind != "voice" or item.start_at > now
                        or item.id in skipped or not item.meta.get("text")):
                    continue
                history[item.id] = {"id": item.id, "host": item.meta.get("host", "Host"),
                                    "text": item.meta["text"], "start_at": item.start_at,
                                    "end_at": item.end_at,
                                    **({"reference": item.meta["reference"]}
                                       if item.meta.get("reference") else {})}
            rows = sorted(history.values(), key=lambda r: (r["start_at"], r["id"]))[-200:]
            self._transcript = {row["id"]: row for row in rows}
            live_ids = {i.id for i in self.schedule.items if i.kind == "voice"}
            return [{**row, "active": row["id"] in live_ids and row["start_at"] <= now < row["end_at"]}
                    for row in rows]

    def now_playing(self) -> dict[str, Any] | None:
        with self.lock:
            now = self.clock.now()
            playing = [i for i in self.schedule.music_items()
                       if i.start_at <= now < i.end_at]
        if not playing:
            return None
        current = max(playing, key=lambda i: i.start_at)
        return {**current.meta, "position": round(now - current.start_at, 2),
                "duration": round(current.duration, 2)}

    def skip(self) -> dict[str, Any]:
        """Wind forward to just before the next transition.

        A skip that cut the record dead and started the next one would throw
        away the transition -- the blend, the bass swap, the filter sweep, and
        any link the hosts had written over it. Instead the station clock is
        moved forward to a moment before the crossfade begins, so what you
        hear is the mix you would have heard anyway, just sooner.

        If the next song is still being prepared, keep playing and remember
        the skip until its handoff is ready. Never destroy the current pair.
        """
        with self.lock:
            return self._skip_locked()

    def _service_deferred_skip(self):
        pending = getattr(self, "_deferred_skip", None)
        if not pending or self.clock.now() < pending[1]:
            return
        self._deferred_skip = None
        if any(i.id == pending[0] and i.start_at <= self.clock.now() < i.end_at
               for i in self.schedule.music_items()):
            self._skip_locked(record=False)

    def _skip_locked(self, record: bool = True) -> dict[str, Any]:
        now = self.clock.now()
        music = sorted(self.schedule.music_items(), key=lambda i: i.start_at)
        current = [i for i in music if i.start_at <= now < i.end_at]
        if len(current) > 1:
            self._pending_skip = None
            return {"ok": True, "mode": "already_mixing",
                    "into": current[-1].meta.get("title"), "skipped": 0}

        item = current[-1] if current else None
        if item:
            # Skipped records never count as heard out, whatever happens next.
            skipped_items = getattr(self, "_skipped_items", set())
            skipped_items.add(item.id)
            self._skipped_items = skipped_items
        if record and item and getattr(self, "_pending_skip", None) != item.id:
            key = item.meta.get("key")
            if key:
                taste.record("skipped", key, item.meta.get("artist", ""),
                             position=item.offset + timeline.playback.source_at(
                                 item.meta.get("rate_curve"), now - item.start_at,
                                 item.meta.get("playback_rate", 1.0)))

        groups = []
        for voice in sorted((i for i in self.schedule.items if i.kind == "voice"), key=lambda i: i.start_at):
            if groups and voice.start_at <= groups[-1][1] + 3.0:
                groups[-1][1] = max(groups[-1][1], voice.end_at)
            else:
                groups.append([voice.start_at, voice.end_at])
        active = next((g for g in groups if g[0] <= now < g[1]), None)
        if active and item:
            self._pending_skip = item.id
            self._deferred_skip = (item.id, active[1] + 0.1)
            return {"ok": True, "mode": "speaking", "skipped": 0,
                    "message": "Letting the hosts finish, then moving to the transition."}
        lead = float(config.station.get("skip.lead_in", 10.0))
        lead = max(2.0, min(30.0, lead)) if math.isfinite(lead) else 10.0
        upcoming = [i for i in music if i.start_at > now]
        if upcoming:
            self._pending_skip = None
            target = max(now, upcoming[0].start_at - lead)
            for start, end in groups:
                if start <= upcoming[0].start_at and end >= target:
                    target = max(now, min(target, start - 1.0))
            if target > now + 0.1:
                self.transcript()
                skipped = getattr(self, "_skipped_speech", set())
                skipped.update(i.id for i in self.schedule.items
                               if i.kind == "voice" and now < i.start_at and i.end_at <= target)
                self._skipped_speech = skipped
                self.clock.jump(target - now)
                self._epoch += 1
                return {"ok": True, "mode": "transition",
                        "into": upcoming[0].meta.get("title"),
                        "skipped": round(target - now, 1)}
            return {"ok": True, "mode": "already_mixing",
                    "into": upcoming[0].meta.get("title"), "skipped": 0}

        self._pending_skip = item.id if item else None
        self.status_note = "preparing the next transition" if item else "waiting on the next track"
        return {"ok": True, "mode": "preparing" if item else "empty", "into": None, "skipped": 0}

    def rate_current(self, value: str = "down", skip: bool = True) -> bool:
        """Mark whatever is on air right now. Used by 'I hate this'.

        Skips as well by default: typing that means you want it gone, not
        just noted for later.
        """
        playing = self.now_playing()
        key = (playing or {}).get("key")
        if not key:
            return False
        taste.record(f"thumbs_{value}", key, (playing or {}).get("artist", ""))
        if skip and value == "down":
            self.skip()
        return True

    def report(self, kind: str, key: str, position: float = 0.0,
               duration: float = 0.0, item_id: str = "") -> None:
        """Playback feedback from either player, counted once per airing."""
        if not key:
            return
        row = db.one("SELECT artist FROM tracks WHERE key=?", (key,))
        artist = row["artist"] if row else ""
        if kind == "started":
            with self.lock:
                now = self.clock.now()
                self._finish_requests(now)
                current = next((i for i in reversed(self.schedule.music_items())
                                if i.meta.get("key") == key and (not item_id or i.id == item_id)
                                and i.start_at <= now < i.end_at), None)
                if item_id and current is None:
                    return  # Stale report after a skip/replaced schedule.
                token = current.id if current else item_id
                reported = getattr(self, "_reported_plays", set())
                if token and token in reported:
                    return
                if token:
                    reported.add(token)
                self._reported_plays = reported
                taste.mark_played(key)
                origin = current.meta.get("selection_origin") if current else {"by": "unknown"}
            db.log_event("played", key, 0.0, selection_origin=origin)
            return
        if kind == "played":
            with self.lock:
                now = self.clock.now()
                aired = [i for i in self.schedule.music_items()
                         if i.meta.get("key") == key and (not item_id or i.id == item_id)
                         and i.start_at <= now]
                item = max(aired, key=lambda i: i.start_at) if aired else None
                credited = getattr(self, "_credited_items", {})
                if item is not None:
                    if item.id in credited or item.id in getattr(self, "_skipped_items", set()):
                        return  # already inferred at its end, or skipped
                    credited[item.id] = key
                    self._credited_items = credited
        taste.record(kind, key, artist, position=position, duration=duration)


_STATION: Station | None = None
_INIT_LOCK = threading.Lock()


def station() -> Station:
    global _STATION
    with _INIT_LOCK:
        if _STATION is None:
            _STATION = Station()
            _STATION.start()
    return _STATION
