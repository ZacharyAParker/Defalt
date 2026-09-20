"""Explicit ad requests, prepared off-thread and inserted without moving music."""
from __future__ import annotations

import threading
import uuid

from . import config, db, intent, timeline
from .segments import writers


class AdBreaks:
    def __init__(self, station):
        self.station = station
        self.request = None
        self.voices = []

    def public(self):
        with self.station.lock:
            self.tick()
            result = dict(self.request or {})
            result.pop("items", None)
            result["enabled"] = bool(config.games.get("ads.enabled", True))
            result["busy"] = result.get("state") in {"preparing", "ready", "scheduled", "playing"}
            return result

    def queue(self, timing, *, brief='', news_category=''):
        if timing not in ("next_break", "now"):
            raise ValueError("Choose next_break or now.")
        if not isinstance(brief,str) or len(brief) > 1200:
            raise ValueError('Ad briefs must be at most 1,200 characters.')
        brief=intent.clean(brief)
        if brief and (error := intent.screen(brief,max_chars=1200)):
            raise ValueError(error)
        if not isinstance(news_category,str):
            raise ValueError('Choose a news category for the ad.')
        if news_category:
            category=(config.news.get('categories',{}) or {}).get(news_category)
            if not brief or not isinstance(category,dict) or not category.get('enabled') or not category.get('feeds'):
                raise ValueError('That news category is unavailable. Enable its feeds or request an ad without news.')
        s = self.station
        with s.lock:
            if not config.games.get("ads.enabled", True):
                raise ValueError("Ads are disabled in config/games.yaml.")
            if not s.clock.running:
                raise ValueError("Start radio playback before requesting an ad.")
            if self.public()["busy"]:
                if brief and (brief != self.request.get('brief') or news_category != self.request.get('news_category') or timing != self.request.get('timing')):
                    raise ValueError('An ad is already preparing or queued. Your new brief was not added; let that ad finish first.')
                return self.public()
            token = uuid.uuid4().hex
            self.request = {"id": token, "timing": timing, "state": "preparing",
                            "message": "Writing and voicing an ad...", "brief":brief,
                            "news_category":news_category}
            self.voices = []
            thread = threading.Thread(target=self._prepare, args=(token,), daemon=True,
                                      name="ad-preparation")
            thread.start()
            return self.public()

    def _prepare(self, token):
        s = self.station
        try:
            with s.lock:
                if not self.request or self.request['id'] != token:
                    return
                context = writers.build_context("game_ad", previous=s._last_track,
                                                recent_host_lines=list(getattr(s, "_recent_host_lines", []))[-16:])
                context['ad_brief']=self.request.get('brief','')
                context['ad_news_category']=self.request.get('news_category','')
            lines = writers.game_ad(context)
            if not lines:
                raise ValueError("No ad material is available. Enable house ads or add a game to the watchlist.")
            voices = s._render(lines)
            if len(voices) != len(lines) or any(v.duration <= 0 for v in voices):
                raise ValueError("The ad's voice render failed. Try again.")
            with s.lock:
                if not self.request or self.request["id"] != token or s._stop.is_set():
                    return
                self.voices = voices
                self.request.update(state="ready", message="Ad ready for the next host break.",
                                    copy_source=context.get('_ad_copy_source','generated'),
                                    product=(context.get("_ad") or {}).get("name", "House ad"))
                if context.get("_ad"):
                    writers.steam.mark_ad_used(context["_ad"])
                self.tick()
        except Exception as error:
            with s.lock:
                if self.request and self.request["id"] == token:
                    self.request.update(state="failed", message=f"Ad failed: {str(error)[:220]}")

    def tick(self):
        """Called under the station lock; never performs network or synthesis work."""
        request = self.request
        if not request:
            return
        s, now = self.station, self.station.clock.now()
        if request["state"] in {"scheduled", "playing"}:
            if not any(i.id in request["items"] for i in s.schedule.items):
                request.update(state="skipped", message="Ad left the schedule. You can queue another.")
            elif now >= request["end_at"]:
                request.update(state="done", message="Ad finished." if request["state"] == "playing" else "Ad passed during a skip.")
            elif now >= request["start_at"]:
                if request["state"] != "playing":
                    db.mark_aired("game_ad", manual=True, product=request.get("product"))
                request.update(state="playing", message=f"On air: {request.get('product', 'Ad break')}")
            return
        if request["state"] != "ready" or not s.clock.running:
            return
        # Leave time for clients to poll and decode the new voice files.
        earliest = now + 5.0
        speech = sorted((i for i in s.schedule.items if i.kind == "voice" and i.end_at > now),
                        key=lambda i: i.start_at)
        laid = timeline.lay_out_lines(self.voices, 0., s.rng)
        first, last = timeline.speech_span(laid)
        length = last - first
        if request["timing"] == "next_break":
            if not speech:
                return  # The builder will call again when the next break is placed.
            # Join the end of the next talk segment, preserving its existing lines.
            end = speech[0].end_at
            for item in speech[1:]:
                if item.start_at > end + 3.0:
                    break
                end = max(end, item.end_at)
            start = max(earliest, end + 0.6)
        else:
            start = earliest
        # Never interrupt a host or collide with speech already handed to a client.
        for item in speech:
            if item.start_at < start + length + 0.6 and item.end_at + 0.6 > start:
                start = item.end_at + 0.6
        ids = []
        for line, offset in laid:
            item = s.schedule.add_voice(line.url, start + offset - first, line.duration,
                                        gain=line.gain, meta={"host": line.host, "text": line.text,
                                        "segment": "Ad break", "ad_request": request["id"]})
            ids.append(item.id)
        # Seal derives ducks from speech on both decks, including future appends.
        s.schedule.seal()
        s._recent_host_lines = (getattr(s, "_recent_host_lines", []) + [v.text for v in self.voices])[-32:]
        source_note = ' (backup script; fresh writing was unavailable)' if request.get('copy_source') == 'backup' else ''
        request.update(state="scheduled", start_at=start, end_at=start + length, items=ids,
                       message=f"Ad queued: {request.get('product', 'House ad')}{source_note}")
        self.voices = []


def for_station(station):
    with station.lock:
        if not hasattr(station, "ads"):
            station.ads = AdBreaks(station)
        return station.ads
