"""What time it is, said the way a host would say it, plus optional weather.

Everything here is local and cheap except weather, which is off unless a
latitude and longitude are configured, cached for half an hour, and given a
short timeout so a slow forecast can never hold up a break.
"""
from __future__ import annotations

import threading
import time
from typing import Any

import httpx

from . import config

_ONES = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight",
         "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen",
         "sixteen", "seventeen", "eighteen", "nineteen"]
_TENS = {2: "twenty", 3: "thirty", 4: "forty", 5: "fifty"}


def _number(value: int) -> str:
    if value < 20:
        return _ONES[value]
    tens, ones = divmod(value, 10)
    return _TENS[tens] + (f" {_ONES[ones]}" if ones else "")


def _local(when: float | None) -> time.struct_time:
    return time.localtime(time.time() if when is None else when)


def daypart(when: float | None = None) -> str:
    """morning / afternoon / evening / late night -- the four a host would use."""
    hour = _local(when).tm_hour
    if 5 <= hour < 12:
        return "morning"
    if 12 <= hour < 17:
        return "afternoon"
    if 17 <= hour < 21:
        return "evening"
    return "late night"


def spoken_time(when: float | None = None) -> str:
    """"seven oh five in the evening", "midnight", "noon" -- never "7 05"."""
    now = _local(when)
    hour, minute = now.tm_hour, now.tm_min
    if minute == 0 and hour == 0:
        return "midnight"
    if minute == 0 and hour == 12:
        return "noon"
    hour12 = hour % 12 or 12
    part = ("in the morning" if 5 <= hour < 12
            else "in the afternoon" if 12 <= hour < 17
            else "in the evening" if 17 <= hour < 21
            else "at night")
    if minute == 0:
        return f"{_number(hour12)} o'clock {part}"
    if minute < 10:
        return f"{_number(hour12)} oh {_number(minute)} {part}"
    return f"{_number(hour12)} {_number(minute)} {part}"


def spoken_date(when: float | None = None) -> str:
    now = _local(when)
    return time.strftime("%A, %B ", now) + str(now.tm_mday)


def voice_daypart(when: float | None = None) -> str:
    """The adjective TTS directions use: only "late-night" when it is night."""
    return {"late night": "late-night"}.get(daypart(when), daypart(when))


# --------------------------------------------------------------------------
# Weather (Open-Meteo, no key). Off unless weather.latitude/longitude are set.
# --------------------------------------------------------------------------
_WEATHER_LOCK = threading.Lock()
_WEATHER: dict[str, Any] = {}
FORECAST = "https://api.open-meteo.com/v1/forecast"

# WMO weather interpretation codes, grouped into things a host would say.
_CODES = {0: "clear", 1: "mostly clear", 2: "partly cloudy", 3: "overcast",
          45: "foggy", 48: "foggy", 51: "drizzle", 53: "drizzle", 55: "heavy drizzle",
          56: "freezing drizzle", 57: "freezing drizzle", 61: "light rain", 63: "rain",
          65: "heavy rain", 66: "freezing rain", 67: "freezing rain", 71: "light snow",
          73: "snow", 75: "heavy snow", 77: "snow grains", 80: "rain showers",
          81: "rain showers", 82: "violent rain showers", 85: "snow showers",
          86: "heavy snow showers", 95: "a thunderstorm", 96: "a thunderstorm with hail",
          99: "a thunderstorm with hail"}


def _weather_settings() -> tuple[float, float, str] | None:
    settings = config.station.get("weather", {}) or {}
    if not isinstance(settings, dict) or settings.get("enabled") is False:
        return None
    try:
        latitude, longitude = float(settings["latitude"]), float(settings["longitude"])
    except (KeyError, TypeError, ValueError):
        return None
    if not (-90 <= latitude <= 90 and -180 <= longitude <= 180):
        return None
    units = "fahrenheit" if str(settings.get("units", "celsius")).lower().startswith("f") else "celsius"
    return latitude, longitude, units


def weather(now: float | None = None) -> str | None:
    """A short description such as "light rain, 12 degrees celsius", or None."""
    settings = _weather_settings()
    if not settings:
        return None
    now = time.time() if now is None else now
    with _WEATHER_LOCK:
        cached = _WEATHER.get(settings)
        # Failures are cached too, for a shorter time, so an outage costs one
        # short timeout every ten minutes rather than one per break.
        if cached and now - cached[0] < (1800 if cached[1] else 600):
            return cached[1]
    latitude, longitude, units = settings
    try:
        response = httpx.get(FORECAST, params={
            "latitude": latitude, "longitude": longitude,
            "current": "temperature_2m,weather_code", "temperature_unit": units,
        }, timeout=4.0, headers={"User-Agent": "personal-radio/1.0"})
        response.raise_for_status()
        current = response.json().get("current") or {}
        temperature = round(float(current["temperature_2m"]))
        sky = _CODES.get(int(current.get("weather_code", -1)))
        value = f"{sky + ', ' if sky else ''}{temperature} degrees {units}"
    except (httpx.HTTPError, ValueError, KeyError, TypeError) as error:
        if config.DEBUG:
            print(f"[weather] unavailable: {type(error).__name__}", flush=True)
        value = None
    with _WEATHER_LOCK:
        _WEATHER[settings] = (now, value)
    return value
