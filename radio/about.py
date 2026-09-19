"""Public release documents shared by the repository and player."""
import tomllib

from .config import ROOT

VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
COPYRIGHT = "© 2026 Zachary Parker"
DOCUMENTS = {
    "patches": "CHANGELOG.md",
    "privacy": "PRIVACY.md",
    "terms": "TERMS.md",
    "copyright": "COPYRIGHT.md",
}


def release_info():
    return {
        "version": VERSION,
        "copyright": COPYRIGHT,
        "documents": {name: (ROOT / filename).read_text(encoding="utf-8")
                      for name, filename in DOCUMENTS.items()},
    }
