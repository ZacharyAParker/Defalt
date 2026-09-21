import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from radio import config
from radio.segments import writers


class PartialConfigurationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.base = root / 'station.yaml'
        self.overrides = root / 'overrides.yaml'
        self.base.write_text('hosts:\n  humour: Current house style\n  self_aware: true\n'
                             '  speech:\n    speed: 1\n    volume: 0.8\n', encoding='utf-8')
        self.overrides.write_text('hosts:\n  self_aware: false\n  speech:\n    speed: 1.2\n', encoding='utf-8')
        self.settings = config.OverridableConfig(self.base, self.overrides)

    def test_saved_host_toggle_preserves_house_style_in_real_writer(self):
        with patch.object(config, 'station', self.settings):
            self.assertEqual(writers._humour(), 'Current house style')

    def test_group_reads_match_leaf_reads_and_complete_configuration(self):
        group = self.settings.get('hosts')
        self.assertEqual(group, self.settings.data()['hosts'])
        self.assertEqual(group['humour'], self.settings.get('hosts.humour'))
        self.assertFalse(group['self_aware'])
        self.assertEqual(group['speech'], {'speed': 1.2, 'volume': 0.8})
        self.assertEqual(self.settings.base.get('hosts.speech.speed'), 1)

    def test_explicit_leaf_overrides_still_win_and_null_is_preserved(self):
        self.settings.set_many({'hosts.humour': 'Custom style', 'hosts.speech.volume': None})
        self.assertEqual(self.settings.get('hosts')['humour'], 'Custom style')
        self.assertIsNone(self.settings.get('hosts.speech')['volume'])
        self.assertEqual(self.settings.get('missing', 'fallback'), 'fallback')

    def test_updated_base_style_is_not_hidden_by_saved_sibling(self):
        self.settings.get('hosts')
        self.base.write_text('hosts:\n  humour: A revised house style\n  self_aware: true\n', encoding='utf-8')
        self.settings.base._mtime = None
        with patch.object(config, 'station', self.settings):
            self.assertEqual(writers._humour(), 'A revised house style')
