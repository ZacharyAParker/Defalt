"""Conservative edition labels; ordinary titles and artists are not ratings."""
import re


_CLEAN = r"(?:clean|censored|non[\s-]?explicit|radio[\s-]friendly)"
_EXPLICIT = r"(?:explicit|uncensored)"
_SUFFIX = r"(?:\s+(?:version|edit|edition|audio|lyrics?|mix))*"


def _tagged(text: str, marker: str) -> bool:
    # Require an edition-shaped label, not a word in a song/artist name.
    return bool(re.search(
        rf"(?:[\[(]\s*{marker}\b{_SUFFIX}\s*[\])]|"
        rf"\s[-–—]\s*{marker}\b{_SUFFIX}\s*$|"
        rf"\S\s+{marker}\s+(?:version|edit|edition)\b)", text, re.I))


def is_clean_label(text: str) -> bool:
    return _tagged(text or "", _CLEAN)


def source_edition(name: str, artist: str, title: str) -> tuple[bool, bool]:
    """Identify edition words left after removing the requested song identity.

    Remove each identity once, so 'Clean (Clean Version)' still has a label.
    Do not scan uploader names or description links advertising other editions.
    """
    residual = name or ""
    for identity in (artist, title):
        if identity:
            residual = re.sub(r"(?<!\w)" + re.escape(identity) + r"(?!\w)",
                              "", residual, count=1, flags=re.I)
    clean = (is_clean_label(title) and is_clean_label(name)) or bool(
        re.search(rf"\b{_CLEAN}\b", residual, re.I))
    explicit = (_tagged(title, _EXPLICIT) and _tagged(name, _EXPLICIT)) or bool(
        re.search(rf"\b{_EXPLICIT}\b", residual, re.I))
    # 'Non-explicit' contains 'explicit'; it must never receive an explicit bonus.
    return clean, explicit and not clean


def clean_track(track: dict) -> bool:
    return any(is_clean_label(track.get(field) or "") for field in ("title", "album"))
