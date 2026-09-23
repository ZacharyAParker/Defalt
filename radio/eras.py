"""Release years and eras, read out of what the listener typed.

"songs from 2010-2015", "90s r&b", "some 2016 bangers", "early 2010s",
"the Obama era". Deterministic on purpose: an era request must work when the
model is rate limited, and a wrong guess queues the wrong decade.

Years are inclusive [first, last] pairs. Unknown release years are always
neutral in scoring; only an explicit catalog request filters on them.
"""
from __future__ import annotations

import re
import time

EARLIEST = 1900


def latest() -> int:
    return time.localtime().tm_year + 1


_WORD_DECADES = {
    "twenties": 1920, "thirties": 1930, "forties": 1940, "fifties": 1950,
    "sixties": 1960, "seventies": 1970, "eighties": 1980, "nineties": 1990,
    "noughties": 2000, "aughts": 2000, "two thousands": 2000, "tens": 2010,
    "twenty tens": 2010, "twenty-tens": 2010,
}
# Named eras people actually say. Bounded and unambiguous; anything fancier
# is a question for the listener, not a guess.
_NAMED = {
    r"obama(?:\s+|-)era|obama years": (2009, 2016),
    r"bush(?:\s+|-)era|bush years": (2001, 2008),
    r"trump(?:\s+|-)era|trump years": (2017, 2020),
    r"y2k(?:\s+era)?": (1998, 2002),
    r"(?:the\s+)?millennium": (1998, 2002),
}
_PART = {"early": (0, 3), "mid": (3, 6), "middle": (3, 6), "late": (6, 9)}

_YEAR = r"(?:1[89]\d\d|20\d\d)"
_RANGE = re.compile(
    rf"(?:\b(?:from|between)\s+)?\b(?P<a>{_YEAR})\s*(?:-|–|—|to|through|thru|until|till|and)\s*(?P<b>{_YEAR}|\d\d)\b",
    re.IGNORECASE)
_DECADE = re.compile(
    r"(?:\b(?P<part>early|mid|middle|late)[\s-]+)?"
    r"(?:(?<![\w'’])(?P<full>(?:1[89]|20)\d0)'?s\b|(?<![\w])['’]?(?P<short>\d0)'?s\b)",
    re.IGNORECASE)
_WORDS = re.compile(
    r"(?:\b(?P<part>early|mid|middle|late)[\s-]+)?\b(?P<word>" + "|".join(
        sorted((re.escape(w) for w in _WORD_DECADES), key=len, reverse=True)) + r")\b",
    re.IGNORECASE)
_SINGLE = re.compile(rf"(?<![\w-])(?P<year>{_YEAR})(?![\w-])")
_RELATIVE = re.compile(r"\b(?P<which>this|last)\s+year'?s?\b", re.IGNORECASE)
_SINCE = re.compile(rf"\b(?:since|after)\s+(?P<year>{_YEAR})\b", re.IGNORECASE)
_BEFORE = re.compile(rf"\bbefore\s+(?P<year>{_YEAR})\b", re.IGNORECASE)


def _valid(first: int, last: int) -> tuple[int, int] | None:
    first, last = min(first, last), max(first, last)
    if first < EARLIEST or last > latest():
        return None
    return (first, last)


def _short_decade(digits: int) -> int:
    """'90s' is 1990, '20s' is the 2020s: the recent one people mean on a radio."""
    return (1900 + digits) if digits >= 30 else (2000 + digits)


def _decade(start: int, part: str | None) -> tuple[int, int]:
    if part:
        low, high = _PART[part.lower()]
        return (start + low, start + high)
    return (start, start + 9)


def parse(text: str) -> tuple[int, int] | None:
    """The first year range the text names, or None. Never guesses."""
    text = str(text or "")
    if not text.strip():
        return None
    for pattern, span in _NAMED.items():
        if re.search(r"\b(?:" + pattern + r")\b", text, re.IGNORECASE):
            return _valid(*span)
    match = _RANGE.search(text)
    if match:
        first = int(match["a"])
        second = match["b"]
        last = int(second) if len(second) == 4 else (first // 100) * 100 + int(second)
        if len(second) == 2 and last < first:
            last += 100
        return _valid(first, last)
    match = _SINCE.search(text)
    if match:
        return _valid(int(match["year"]), latest() - 1)
    match = _BEFORE.search(text)
    if match:
        year = int(match["year"])
        return _valid(max(EARLIEST, year - 10), year - 1)
    match = _DECADE.search(text)
    if match:
        start = int(match["full"]) if match["full"] else _short_decade(int(match["short"]))
        return _valid(*_decade(start, match["part"]))
    match = _WORDS.search(text)
    if match:
        return _valid(*_decade(_WORD_DECADES[match["word"].lower()], match["part"]))
    match = _RELATIVE.search(text)
    if match:
        year = latest() - 1 - (1 if match["which"].lower() == "last" else 0)
        return (year, year)
    match = _SINGLE.search(text)
    if match:
        year = int(match["year"])
        return _valid(year, year)
    return None


def strip(text: str) -> str:
    """The text with its era phrase removed, for finding genres beside it."""
    text = str(text or "")
    for pattern in [*(r"\b(?:" + p + r")\b" for p in _NAMED), _RANGE.pattern, _SINCE.pattern,
                    _BEFORE.pattern, _DECADE.pattern, _WORDS.pattern, _RELATIVE.pattern, _SINGLE.pattern]:
        text = re.sub(pattern, " ", text, flags=re.IGNORECASE)
    text = re.sub(r"\b(?:from|in|of|the|era|years?)\b", " ", text, flags=re.IGNORECASE)
    return re.sub(r"\s+", " ", text).strip(" ,.-")


def coerce(value) -> tuple[int, int] | None:
    """Accept [from, to], a single year, or a phrase such as 'early 2010s'."""
    if value is None or value == "" or value == []:
        return None
    if isinstance(value, str):
        return parse(value)
    if type(value) is int:
        return _valid(value, value)
    if isinstance(value, (list, tuple)) and 1 <= len(value) <= 2:
        # Models sometimes quote numbers; a four-digit string is still a year.
        years = [int(v) if isinstance(v, str) and re.fullmatch(r"\d{4}", v.strip()) else v for v in value]
        if all(type(v) is int for v in years):
            return _valid(years[0], years[-1])
    raise ValueError("Years must be a year, a [from, to] pair, or a decade such as '90s'.")


def label(years) -> str:
    if not years:
        return ""
    first, last = years
    return str(first) if first == last else f"{first}–{last}"


def year_of(track) -> int | None:
    """A plausible release year from a track row, else None."""
    try:
        value = track.get("year") if hasattr(track, "get") else track["year"]
    except (KeyError, IndexError, TypeError):
        return None
    try:
        year = int(str(value)[:4])
    except (TypeError, ValueError):
        return None
    return year if EARLIEST <= year <= latest() else None


def fit(year: int | None, years) -> float | None:
    """1 inside the range, easing to 0 about eight years outside. None if unknown."""
    if year is None or not years:
        return None
    first, last = years
    if first <= year <= last:
        return 1.0
    distance = first - year if year < first else year - last
    return max(0.0, 0.6 - distance * 0.075)
