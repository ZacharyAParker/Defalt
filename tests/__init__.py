"""Tests never touch the real station.

Credentials in a working checkout would otherwise switch on live Spotify,
OpenRouter or Steam calls inside tests that expect them off, as they are in
CI, and saved Mix settings or the listening database would quietly change
what a test sees (or get written to). This runs before anything in radio
is imported: the .env is skipped, and the settings override layer and the
default database point at a throwaway folder. Tests that need their own
database or settings still patch them as before.
"""
import atexit
import shutil
import tempfile
from pathlib import Path

import dotenv

dotenv.load_dotenv = lambda *args, **kwargs: False

_scratch = Path(tempfile.mkdtemp(prefix="defalt-tests-"))
atexit.register(shutil.rmtree, _scratch, ignore_errors=True)

from radio import config, db  # noqa: E402  (after the .env is switched off)

config.station.override = config.ConfigFile(_scratch / "overrides.yaml")
config.station._merged = None
db._DB_PATH = _scratch / "station.db"
