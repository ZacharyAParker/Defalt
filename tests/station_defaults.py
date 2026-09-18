"""Music-rule tests use defaults and never write the listener's live settings."""
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from radio import config


class StationDefaults(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        station = config.OverridableConfig(config.CONFIG_DIR / "station.yaml",
                                          Path(directory.name) / "overrides.yaml")
        p = patch.object(config, "station", station)
        p.start()
        self.addCleanup(p.stop)
