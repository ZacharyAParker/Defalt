import json
import sqlite3
import unittest
from unittest.mock import patch

from radio import ad_copy
from radio.segments.base import Line


class AdCopyTests(unittest.TestCase):
    def setUp(self):
        self.connection = sqlite3.connect(':memory:')
        self.connection.row_factory = sqlite3.Row
        self.connection.execute('CREATE TABLE events(id INTEGER PRIMARY KEY, ts REAL, kind TEXT, meta TEXT)')
        self.mock = patch.object(ad_copy.db, 'connect', return_value=self.connection)
        self.mock.start()
        self.addCleanup(self.mock.stop)
        self.addCleanup(self.connection.close)

    def generate(self, subject, text=None):
        proposal = ad_copy.plan(subject, ['deadpan', 'interview'])
        lines = text or ad_copy.fallback(subject, proposal['history'])
        return ad_copy.finish(subject, proposal, [Line('mav' if i%2 else 'rue', t) for i,t in enumerate(lines)], 'mav', 'rue')

    def test_each_house_product_has_six_distinct_offline_reads(self):
        for name in ad_copy.BITS:
            subject = {'name':name, 'fictional':True}
            reads = [ad_copy.normalized([line.text for line in self.generate(subject)]) for _ in range(6)]
            self.assertEqual(len(set(reads)), 6, name)

    def test_repeated_model_copy_is_replaced_with_fresh_copy(self):
        subject = {'name':'Queue Insurance', 'fictional':True}
        original = ['I prepared something completely original.', 'This joke has already aired.', 'Send help.', 'Unsponsored.']
        first = self.generate(subject, original)
        second = self.generate(subject, original)
        self.assertNotEqual([l.text for l in first], [l.text for l in second])
        self.assertEqual(len(ad_copy.recent()), 2)

    def test_history_loaded_from_database_controls_next_premise(self):
        subject = {'name':'Grass Touch Simulator', 'fictional':True}
        old = {'product':subject['name'], 'style':'deadpan', 'angle':ad_copy.ANGLES[0],
               'lines':ad_copy.BITS[subject['name']][0] + [ad_copy.CLOSES[0]]}
        self.connection.execute('INSERT INTO events(kind,meta) VALUES(?,?)', ('ad_prepared',json.dumps(old)))
        proposal = ad_copy.plan(subject, ['deadpan','interview'])
        self.assertEqual(proposal['style'], 'interview')
        self.assertNotEqual(proposal['angle'], old['angle'])
        self.assertNotEqual(ad_copy.fallback(subject, proposal['history'])[0], old['lines'][0])

    def test_real_product_is_never_described_as_invented(self):
        lines = ad_copy.fallback({'name':'An Actual Game'}, [])
        self.assertIn('Unsponsored', lines[-1])
        self.assertNotIn('invented', ' '.join(lines).lower())
        self.assertNotIn('fictional', ' '.join(lines).lower())

    def test_closing_rotates_even_when_history_window_is_full(self):
        subject = {'name':'Queue Insurance', 'fictional':True}
        self.generate(subject)
        history = ad_copy.recent() * 24
        first = ad_copy.fallback(subject, history)
        history.insert(0, {'lines':first})
        second = ad_copy.fallback(subject, history[:24])
        self.assertNotEqual(first[-1], second[-1])
