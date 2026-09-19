"""Skipping can be transport-only while explicit ratings still teach the station."""
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from radio import config, db, mixconfig, taste
from radio.segments import base, personal, writers


class SkipPreferences(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        local = threading.local()
        self.settings = {'learning.ignore_skips':True, 'learning.signal_weights':{
            'skipped_early':-1.8, 'skipped_late':-.4, 'thumbs_up':3, 'thumbs_down':-4}}
        for p in [patch.object(db,'_DB_PATH',Path(temp.name)/'station.db'),
                  patch.object(db,'_LOCAL',local),
                  patch.object(config.station,'get',side_effect=lambda k,d=None:self.settings.get(k,d))]:
            p.start(); self.addCleanup(p.stop)
        self.addCleanup(lambda: getattr(local,'conn',None) and local.conn.close())
        db.write("INSERT INTO tracks(key,title,artist,play_count,skip_count,added_at) VALUES('song','A Record','An Artist',3,7,0)")

    def test_ignored_skips_do_not_change_affinity_counts_or_history(self):
        taste.bump('song','An Artist',4)
        score = taste.affinity('track','song')
        for kind in ['skipped','skipped_early','skipped_late']:
            taste.record(kind,'song','An Artist',position=10,duration=180)
        self.assertAlmostEqual(taste.affinity('track','song'),score,places=5)
        row = db.one("SELECT play_count,skip_count FROM tracks WHERE key='song'")
        self.assertEqual((row['play_count'],row['skip_count']),(3,7))
        self.assertFalse(db.query('SELECT * FROM events'))

    def test_explicit_ratings_still_change_taste(self):
        taste.record('thumbs_down','song','An Artist')
        self.assertAlmostEqual(taste.affinity('track','song'),-4,places=5)
        taste.record('thumbs_up','song','An Artist')
        self.assertAlmostEqual(taste.affinity('track','song'),-1,places=5)

    def test_turning_setting_off_restores_skip_learning(self):
        self.settings['learning.ignore_skips']=False
        taste.record('skipped','song','An Artist',position=10)
        self.assertAlmostEqual(taste.affinity('track','song'),-1.8,places=5)
        self.assertEqual(db.one("SELECT skip_count FROM tracks WHERE key='song'")['skip_count'],8)

    def test_old_skip_history_and_jokes_are_absent_from_new_personal_briefs(self):
        db.log_event('skipped_early','song')
        db.log_event('request','song')
        track={'key':'song','title':'A Record','artist':'An Artist'}
        data=personal.facts(track)
        self.assertNotIn('early_skips',data)
        self.assertNotIn('late_skips',data)
        with patch.object(personal.memes,'prepare',return_value=None), patch.object(personal,'write',side_effect=lambda b,**kw:kw['fallback']) as writer:
            lines=personal.comment({'next':track,'recent_host_lines':['You skipped it again.']},'mav','rue')
        self.assertNotIn('You skipped it again',writer.call_args.args[0])
        self.assertNotIn('early_skips',writer.call_args.args[0])
        self.assertNotIn('skip', ' '.join(l.text for l in lines).lower())
        self.assertIn('Never mention or joke about the listener skipping',base.system_prompt())

    def test_general_listener_brief_omits_skip_totals(self):
        taste.bump('song','An Artist',3)
        with patch.object(writers,'write',side_effect=lambda b,**kw:kw['fallback']) as writer:
            writers.listener_note({})
        self.assertNotIn('TOTAL SKIPS',writer.call_args.args[0])

    def test_mix_setting_is_a_validated_boolean(self):
        self.assertEqual(mixconfig.validate({'learning.ignore_skips':True}),{'learning.ignore_skips':True})
        with self.assertRaises(ValueError): mixconfig.validate({'learning.ignore_skips':'true'})
