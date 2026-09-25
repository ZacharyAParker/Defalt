"""Public release documents shared by the repository and player."""
import re
import tomllib

from .config import ROOT

VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
COPYRIGHT = "© 2026 Zachary Parker"
DOCUMENTS = {
    "patches": "CHANGELOG.md",
    "privacy": "PRIVACY.md",
    "terms": "TERMS.md",
    "copyright": "COPYRIGHT.md",
    "license": "LICENSE",
    "notices": "THIRD-PARTY-NOTICES.md",
}


def terms_version(text: str | None = None) -> str:
    """The version players ask you to accept. TERMS.md is the only place it's
    written; the console reads the same line when it's built."""
    text = (ROOT / "TERMS.md").read_text(encoding="utf-8") if text is None else text
    found = re.search(r"\(terms version (\d{4}-\d{2}-\d{2})\)", text)
    if not found:
        raise ValueError("TERMS.md has no '(terms version YYYY-MM-DD)' line")
    return found.group(1)


TERMS_VERSION = terms_version()


def release_info():
    return {
        "version": VERSION,
        "copyright": COPYRIGHT,
        "terms_version": TERMS_VERSION,
        "documents": {name: (ROOT / filename).read_text(encoding="utf-8")
                      for name, filename in DOCUMENTS.items()},
    }
