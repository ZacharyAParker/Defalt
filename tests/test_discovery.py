import json
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from radio import config, db, discovery, taste, vibe


class DiscoveryTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        local = threading.local()
        self.settings = {'selection.exploration_rate': .35}
        for mock in (patch.object(db, '_DB_PATH', Path(tmp.name) / 'station.db'),
                     patch.object(db, '_LOCAL', local),
                     patch.object(config.station, 'get', side_effect=lambda k,d=None: self.settings.get(k,d)),
                     patch.object(discovery.spotify, 'available', return_value=True)):
            mock.start(); self.addCleanup(mock.stop)
        self.addCleanup(lambda: getattr(local, 'conn', None) and local.conn.close())
        vibe.set_session_selection(None)
        self.addCleanup(lambda: vibe.set_session_selection(None))
        self.anchor = taste.add_track('Favorite Song', 'Favorite Artist', source='seed')
        taste.bump(self.anchor, 'Favorite Artist', 3)

    def suggestion(self, title='New Song', artist='Related Artist'):
        return {'artist': artist, 'title': title, 'anchor_key': self.anchor, 'reason': 'A related sound'}

    def test_verified_new_music_enters_automatic_pool_without_learning_or_request(self):
        before = [dict(row) for row in db.query('SELECT * FROM affinity')]
        with patch.object(discovery.llm, 'complete_json', return_value=[self.suggestion()]), \
                patch.object(discovery.spotify, 'search', return_value=[{'artist':'Related Artist','title':'New Song','duration_ms':200000}]):
            result = discovery.refresh()
        self.assertEqual(result['added'], 1)
        track = dict(db.one("SELECT * FROM tracks WHERE source='auto_discovery'"))
        self.assertEqual(track['expected_ms'], 200000)
        self.assertEqual(json.loads(track['source_metadata'])['discovery']['anchor']['key'], self.anchor)
        self.assertFalse(db.query('SELECT * FROM requests'))
        self.assertEqual([dict(row) for row in db.query('SELECT * FROM affinity')], before)

    def test_matching_requires_both_artist_and_recording_and_rejects_variants(self):
        for result in [{'artist':'Wrong Artist','title':'New Song'},
                       {'artist':'Related Artist','title':'Wrong Song'},
                       {'artist':'Related Artist','title':'New Song (Acoustic)'}]:
            with patch.object(discovery.spotify, 'search', return_value=[result]):
                self.assertIsNone(discovery.matching_record('Related Artist','New Song'))

    def test_known_blocked_duplicates_disliked_artists_and_fake_anchors_are_excluded(self):
        blocked = taste.add_track('Blocked Song','Blocked Artist')
        db.write('UPDATE tracks SET blocked=1 WHERE key=?', (blocked,))
        taste.bump('missing', 'Disliked Artist', -5)
        bad = [self.suggestion('Favorite Song','Favorite Artist'), self.suggestion('Blocked Song','Blocked Artist'),
               self.suggestion(artist='Disliked Artist'), {**self.suggestion(), 'anchor_key':'invented'}]
        with patch.object(discovery.llm, 'complete_json', return_value=bad), \
                patch.object(discovery.spotify, 'search') as search:
            self.assertEqual(discovery.refresh()['added'], 0)
        search.assert_not_called()

    def test_failure_has_backoff_and_never_inserts_unverified_model_titles(self):
        with patch.object(discovery.llm, 'complete_json', return_value=[self.suggestion()]) as writer, \
                patch.object(discovery.spotify, 'search', side_effect=ValueError('offline')):
            self.assertEqual(discovery.refresh()['added'], 0)
            self.assertEqual(discovery.refresh()['state'], 'cooldown')
        writer.assert_called_once()
        self.assertFalse(db.one("SELECT 1 FROM tracks WHERE source='auto_discovery'"))

    def test_direction_change_during_search_discards_old_suggestion(self):
        def changed(query):
            vibe.set_session_selection({'description':'New direction','genres':['jazz']})
            return [{'artist':'Related Artist','title':'New Song'}]
        with patch.object(discovery.llm, 'complete_json', return_value=[self.suggestion()]), \
                patch.object(discovery.spotify, 'search', side_effect=changed):
            self.assertEqual(discovery.refresh()['added'], 0)

    def test_disabled_and_cancelled_do_not_make_remote_calls(self):
        with patch.object(discovery.llm, 'complete_json') as writer:
            self.assertEqual(discovery.refresh(cancelled=lambda: True)['state'], 'disabled')
            self.settings['selection.discovery_enabled'] = False
            self.assertEqual(discovery.refresh()['state'], 'disabled')
        writer.assert_not_called()

    def test_new_music_share_does_not_vanish_in_large_familiar_library(self):
        old = [(1, {'source':'seed','play_count':0}) for _ in range(200)]
        new = [(1, {'source':'auto_discovery','play_count':0}) for _ in range(2)]
        balanced = discovery.balance(old + new)
        self.assertAlmostEqual(sum(w for w,t in balanced if discovery.unfamiliar(t)), .35)
        self.assertAlmostEqual(sum(w for w,t in balanced), 1)
        self.assertFalse(discovery.unfamiliar({'source':'auto_discovery','play_count':1}))
        for share in (0, 1):
            self.settings['selection.exploration_rate'] = share
            result = discovery.balance(old + new)
            self.assertTrue(all(discovery.unfamiliar(t) == bool(share) for _,t in result))

    def test_full_reserve_does_not_make_remote_calls(self):
        for index in range(12):
            taste.add_track(str(index), 'New Artist', source='auto_discovery')
        with patch.object(discovery.llm, 'complete_json') as writer:
            self.assertEqual(discovery.refresh()['state'], 'ready')
        writer.assert_not_called()
