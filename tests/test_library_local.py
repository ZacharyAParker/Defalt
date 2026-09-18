"""Local library contracts, isolated from the station DB and background threads."""
import os
import shutil
import sqlite3
import tempfile
import threading
import unittest
import wave
from pathlib import Path
from unittest.mock import patch

import numpy as np
from flask import Flask
from mutagen.wave import WAVE
from mutagen.id3 import TIT2, TPE1, TALB, TDRC, TCON

from radio import analysis, config, db, importer, library
from radio.library_api import blueprint


class DatabaseTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.local = threading.local()
        for patcher in (patch.object(db, '_DB_PATH', self.root / 'test.db'),
                        patch.object(db, '_LOCAL', self.local)):
            patcher.start()
            self.addCleanup(patcher.stop)
        self.addCleanup(lambda: getattr(self.local, 'conn', None) and self.local.conn.close())
        db.connect()

    def track(self, key):
        db.write('INSERT INTO tracks(key,title,artist,added_at) VALUES (?,?,?,0)',
                 (key, key, 'Artist'))


class TestCratesAndCues(DatabaseTest):
    def setUp(self):
        super().setUp()
        app = Flask(__name__)
        app.register_blueprint(blueprint)
        self.client = app.test_client()
        self.track('a')
        self.track('b')

    def test_cue_crud_and_eight_slots(self):
        for slot in range(8):
            response = self.client.put(f'/api/tracks/a/cues/{slot}',
                json={'position_samples': slot * 44100, 'colour': '#ff0000', 'label': 'Intro'})
            self.assertEqual(response.status_code, 200)
        self.assertEqual(len(self.client.get('/api/tracks/a/cues').json['cues']), 8)
        self.client.put('/api/tracks/a/cues/0', json={'position_samples': 99})
        self.assertEqual(db.hot_cues('a')[0]['position_samples'], 99)
        self.client.delete('/api/tracks/a/cues/0')
        self.assertEqual(len(db.hot_cues('a')), 7)

    def test_invalid_cues(self):
        for position in (-1, 1.5, True, '10', None, 2**63):
            self.assertEqual(self.client.put('/api/tracks/a/cues/0',
                json={'position_samples': position}).status_code, 400)
        self.assertEqual(self.client.put('/api/tracks/a/cues/8',
            json={'position_samples': 0}).status_code, 400)
        self.assertEqual(self.client.get('/api/tracks/missing/cues').status_code, 404)
        self.assertEqual(self.client.put('/api/tracks/a/cues/0', json=[]).status_code, 400)

    def test_crate_lifecycle_and_atomic_membership(self):
        response = self.client.post('/api/crates', json={'name': 'Set', 'tracks': ['a', 'b']})
        self.assertEqual(response.status_code, 201)
        ident = response.json['id']
        url = f'/api/crates/{ident}'
        response = self.client.put(url, json={'name': 'New', 'tracks': ['b', 'a']})
        self.assertEqual([t['key'] for t in response.json['tracks']], ['b', 'a'])
        self.assertEqual(self.client.put(url, json={'name': 'Bad', 'tracks': ['a', 'missing']}).status_code, 400)
        self.assertEqual(db.crate(ident)['name'], 'New')
        self.assertEqual([t['key'] for t in db.crate(ident)['tracks']], ['b', 'a'])
        self.assertEqual(self.client.put(url, json={'tracks': ['a', 'a']}).status_code, 400)
        self.client.put(url, json={'tracks': ['a']})
        self.assertEqual(len(db.crate(ident)['tracks']), 1)
        self.client.delete(url)
        self.assertEqual(self.client.get(url).status_code, 404)
        self.assertEqual(db.query('SELECT * FROM crate_tracks'), [])

    def test_track_delete_cascades_and_many_to_many(self):
        db.set_hot_cue('a', 0, 0)
        first = db.save_crate('One', ['a', 'b'])
        second = db.save_crate('Two', ['a'])
        db.write('DELETE FROM tracks WHERE key=?', ('a',))
        self.assertEqual(db.hot_cues('a'), [])
        self.assertEqual(len(db.crate(first)['tracks']), 1)
        self.assertEqual(db.crate(second)['tracks'], [])

    def test_migration_is_repeatable_and_preserves_old_rows(self):
        conn = sqlite3.connect(':memory:')
        self.addCleanup(conn.close)
        conn.row_factory = sqlite3.Row
        conn.execute('CREATE TABLE tracks(key TEXT PRIMARY KEY, title TEXT, artist TEXT)')
        conn.execute("INSERT INTO tracks VALUES ('old','Title','Artist')")
        db._migrate(conn)
        db._migrate(conn)
        self.assertEqual(conn.execute('SELECT title FROM tracks').fetchone()[0], 'Title')
        self.assertIn('import_mtime_ns', {r['name'] for r in conn.execute('PRAGMA table_info(tracks)')})


def wav(path, seconds=0.1):
    samples = (np.sin(np.arange(int(44100 * seconds)) * 2 * np.pi * 440 / 44100) * 10000).astype('<i2')
    with wave.open(str(path), 'wb') as output:
        output.setparams((1, 2, 44100, 0, 'NONE', 'not compressed'))
        output.writeframes(samples.tobytes())


class TestImporter(DatabaseTest):
    def test_tagged_flac_without_lyrics_imports_without_mp4_key_error(self):
        from types import SimpleNamespace
        from mutagen._vorbis import VCommentDict
        tags = VCommentDict()
        tags['title'] = ["It's me, It's Verity"]
        tags['artist'] = ['Horror Skunx']
        audio = SimpleNamespace(tags=tags, info=SimpleNamespace(length=123, sample_rate=44100))
        with patch.object(importer.mutagen, 'File', return_value=audio):
            result = importer.import_file(self.path)
        row = db.one('SELECT * FROM tracks WHERE key=?', (result['key'],))
        self.assertEqual(row['artist'], 'Horror Skunx')
        self.assertEqual(row['title'], "It's me, It's Verity")
        self.assertEqual(row['lyrics'], '')

    def setUp(self):
        super().setUp()
        self.path = self.root / '01 Artist - Song.wav'
        wav(self.path)
        for target, value in [('measure', {'samples': [], 'integrated': -20.0})]:
            patcher = patch.object(library, target, return_value=value)
            patcher.start()
            self.addCleanup(patcher.stop)
        patcher = patch.object(analysis, 'profile', return_value={'bpm': 120.0})
        self.profile = patcher.start()
        self.addCleanup(patcher.stop)
        patcher = patch.object(analysis, 'peaks', return_value={})
        self.peaks = patcher.start()
        self.addCleanup(patcher.stop)

    def test_import_skip_update_and_preserve_cues(self):
        first = importer.import_file(self.path)
        row = db.one('SELECT * FROM tracks WHERE key=?', (first['key'],))
        self.assertEqual((row['artist'], row['title'], row['source']), ('Artist', 'Song', 'local'))
        self.assertEqual(row['sample_rate'], 44100)
        db.set_hot_cue(first['key'], 0, 123)
        self.assertEqual(importer.import_file(self.path)['status'], 'skipped')
        self.profile.assert_called_once()
        stamp = self.path.stat().st_mtime_ns + 1000000
        os.utime(self.path, ns=(stamp, stamp))
        updated = importer.import_file(self.path)
        self.assertEqual(updated, {'key': first['key'], 'status': 'updated'})
        self.assertEqual(db.hot_cues(first['key'])[0]['position_samples'], 123)

    def test_filenames_from_the_real_world(self):
        cases = {
            "04 - Alex G - Pretend": ("Alex G", "Pretend"),
            "4829a1a3d1da0b7495feae9a759993f4JACKIE_S_BOX_-_FNAF_MIMIC_SONG":
                ("JACKIE S BOX", "FNAF MIMIC SONG"),
            "@usher  - hey daddy (daddy's home) lyrics":
                ("usher", "hey daddy (daddy's home)"),
            "Boards of Canada - Roygbiv (Official Music Video)":
                ("Boards of Canada", "Roygbiv"),
            "just a title": ("", "just a title"),
        }
        for stem, expected in cases.items():
            with self.subTest(stem=stem):
                self.assertEqual(importer.from_filename(stem), expected)

    def test_untagged_file_still_gets_a_usable_key(self):
        messy = self.root / "0123456789abcdef_Some_Band_-_A_Song_lyrics.wav"
        wav(messy)
        key = importer.import_file(messy)["key"]
        self.assertEqual(key, db.track_key("Some Band", "A Song"))

    def test_tags_override_filenames(self):
        audio = WAVE(self.path)
        audio.add_tags()
        for frame in (TIT2(text=['Tagged']), TPE1(text=['Singer']), TALB(text=['Album']),
                      TDRC(text=['2024-01-02']), TCON(text=['House'])):
            audio.tags.add(frame)
        audio.save()
        data = importer.metadata(self.path)
        self.assertEqual([data[k] for k in ('title', 'artist', 'album', 'year', 'genre')],
                         ['Tagged', 'Singer', 'Album', 2024, 'House'])

    def test_recursive_scan_isolates_bad_files_and_distinct_records(self):
        nested = self.root / 'nested'
        nested.mkdir()
        wav(nested / 'Someone Else - Another Song.wav')
        (nested / 'broken.mp3').write_bytes(b'bad')
        (nested / 'ignore.txt').write_text('ignore')
        report = importer.import_folder(self.root)
        self.assertEqual(report['imported'], 2)
        self.assertEqual(len(report['errors']), 1)
        self.assertEqual(len(db.query('SELECT * FROM tracks')), 2)

    def test_same_record_from_two_paths_collapses(self):
        # Keys are "artist|title", so the same record found in two folders is
        # one row, not two -- otherwise affinity and skip counts would be split
        # across copies and the station would never learn from either.
        nested = self.root / 'nested'
        nested.mkdir()
        copy = nested / self.path.name
        wav(copy)
        report = importer.import_folder(self.root)
        self.assertEqual(report['imported'] + report['updated'], 2)
        rows = db.query('SELECT key, import_path FROM tracks')
        self.assertEqual(len(rows), 1)
        # Most recent import wins, and it points at the file it last read.
        self.assertEqual(rows[0]['import_path'], os.path.normcase(str(copy)))

    def test_analysis_failure_is_retryable(self):
        self.peaks.side_effect = ValueError('decode failed')
        self.assertEqual(len(importer.import_folder(self.root)['errors']), 1)
        self.assertEqual(db.query('SELECT * FROM tracks'), [])
        self.peaks.side_effect = None
        self.assertEqual(importer.import_folder(self.root)['imported'], 1)

    def test_purge_retains_local_reference(self):
        result = importer.import_file(self.path)
        with patch.object(library, 'AUDIO_DIR', self.root / 'cache'):
            library.purge_all()
        self.assertTrue(self.path.exists())
        self.assertEqual(db.one('SELECT file FROM tracks WHERE key=?', (result['key'],))['file'], str(self.path))

    def test_purge_protects_original_inside_cache_directory(self):
        cache = self.root / 'audio'
        cache.mkdir()
        self.path = self.path.rename(cache / self.path.name)
        importer.import_file(self.path)
        fetched = cache / 'fetched.opus'
        fetched.write_bytes(b'audio')
        with patch.object(library, 'AUDIO_DIR', cache):
            library.purge_all()
        self.assertTrue(self.path.exists())
        self.assertFalse(fetched.exists())

    def test_missing_local_file_does_not_resolve_online(self):
        result = importer.import_file(self.path)
        self.path.unlink()
        with patch.object(library, 'resolve') as resolve:
            self.assertIsNone(library.ensure(dict(db.one(
                'SELECT * FROM tracks WHERE key=?', (result['key'],)))))
            resolve.assert_not_called()

    def test_cached_analysis_backfills_all_grid_columns(self):
        result = importer.import_file(self.path)
        db.write('UPDATE tracks SET bpm=NULL WHERE key=?', (result['key'],))
        tonal = {'bpm': 120, 'bpm_confidence': .8, 'key_tonic': 0, 'key_mode': 'major',
                 'key_confidence': .7, 'camelot': '8B', 'beat_offset': .1,
                 'beat_period': .5, 'beat_residual_ms': 4, 'downbeat_offset': .1}
        self.profile.return_value = tonal
        row = library.ensure(dict(db.one('SELECT * FROM tracks WHERE key=?', (result['key'],))))
        self.assertEqual(row['beat_period'], .5)


class TestPeaks(unittest.TestCase):
    def test_extrema_tail_and_silence(self):
        samples = np.zeros(513, dtype=np.float32)
        samples[0], samples[1], samples[-1] = -1, .75, .5
        result = analysis.peak_levels(samples)
        np.testing.assert_equal(result['levels'][512][:, :2], [[-1, .75], [.5, .5]])
        np.testing.assert_equal(result['levels'][32768][0, :2], [-1, .75])
        self.assertTrue(np.all(analysis.peak_levels(np.zeros(1000))['levels'][512] == 0))
        self.assertEqual(analysis.peak_levels(np.zeros(0))['levels'][512].shape, (0, 5))

    def test_frequency_bands_and_parseval(self):
        for frequency, band in [(100, 0), (1000, 1), (5000, 2)]:
            signal = np.sin(np.arange(16385) * 2 * np.pi * frequency / analysis.SAMPLE_RATE)
            result = analysis.peak_levels(signal)
            energy = result['levels'][32768][0, 2:]
            self.assertEqual(int(energy.argmax()), band)
            self.assertAlmostEqual(float(energy.sum()), float(np.mean(signal ** 2)), places=6)

    def test_cache_hit_invalidation_corruption_and_full_decode(self):
        import subprocess
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'song.wav'
            wav(path)
            output = subprocess.CompletedProcess([], 0, np.zeros(22051, dtype='<f4').tobytes(), b'')
            with patch.object(analysis.subprocess, 'run', return_value=output) as decode:
                result = analysis.peaks(path)
                self.assertEqual(result['sample_count'], 22051)
                self.assertNotIn('-t', decode.call_args.args[0])
                cached = analysis.peaks(path)
                decode.assert_called_once()
                np.testing.assert_equal(result['levels'][512], cached['levels'][512])
                stamp = path.stat().st_mtime_ns + 1000000
                os.utime(path, ns=(stamp, stamp))
                analysis.peaks(path)
                self.assertEqual(decode.call_count, 2)
                analysis.peaks_cache(path).write_bytes(b'broken')
                analysis.peaks(path)
                self.assertEqual(decode.call_count, 3)

    @unittest.skipUnless(shutil.which(config.FFMPEG), 'FFmpeg is not installed/on PATH')
    def test_real_ffmpeg_decode(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'song.wav'
            wav(path)
            result = analysis.peaks(path)
            self.assertAlmostEqual(result['sample_count'] / result['sample_rate'], .1, places=3)
            # Cached under the project, not beside the audio: a curated music
            # folder stays free of sidecars.
            self.assertTrue(analysis.peaks_cache(path).exists())
            self.assertEqual(list(path.parent.glob('*.npz')), [])


if __name__ == '__main__':
    unittest.main()
