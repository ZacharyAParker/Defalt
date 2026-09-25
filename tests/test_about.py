"""Release information must be available without starting playback or reading private data."""
import re
import unittest
from datetime import datetime
from unittest.mock import patch

from radio import about
from radio.app import app


class ReleaseInformation(unittest.TestCase):
    def test_documents_and_footer_share_release_version_without_station(self):
        with patch('radio.director.station', side_effect=AssertionError('must stay stopped')):
            client = app.test_client()
            response = client.get('/api/about')
            self.assertEqual(response.status_code, 200)
            data = response.get_json()
            self.assertEqual(set(data), {'version', 'copyright', 'terms_version', 'documents'})
            self.assertEqual(set(data['documents']), {'patches', 'privacy', 'terms', 'copyright', 'license', 'notices'})
            for name, text in data['documents'].items():
                self.assertEqual(text, (about.ROOT / about.DOCUMENTS[name]).read_text(encoding='utf-8'))
                self.assertIn(f'Defalt v{data["version"]}', text)
                self.assertIn(data['copyright'], text)
            page = client.get('/')
            self.assertIn(f'Defalt v{data["version"]}', page.get_data(as_text=True))
            self.assertNotIn('{{APP_VERSION}}', page.get_data(as_text=True))
            self.assertNotIn('{{TERMS_VERSION}}', page.get_data(as_text=True))
            self.assertIn(f'data-terms-version="{data["terms_version"]}"', page.get_data(as_text=True))
            self.assertEqual(page.headers['Cache-Control'], 'no-store')

    def test_request_cannot_select_private_files(self):
        response = app.test_client().get('/api/about?path=.env&document=../../.env')
        self.assertEqual(response.status_code, 200)
        self.assertEqual(set(response.get_json()['documents']), set(about.DOCUMENTS))

    def test_embedded_native_documents_match_backend(self):
        source = (about.ROOT / 'src/ui/about.rs').read_text(encoding='utf-8')
        for filename in about.DOCUMENTS.values():
            self.assertIn(f'include_str!("../../{filename}")', source)
        self.assertIn('env!("CARGO_PKG_VERSION")', source)

    def test_every_page_is_offered_in_both_footers(self):
        native = (about.ROOT / 'src/ui/about.rs').read_text(encoding='utf-8')
        page = (about.ROOT / 'web/index.html').read_text(encoding='utf-8')
        script = (about.ROOT / 'web/static/about.js').read_text(encoding='utf-8')
        footer = page.split('<footer class="app-footer">', 1)[1].split('</footer>', 1)[0]
        tabs = page.split('class="info-window__nav"', 1)[1].split('</nav>', 1)[0]
        for name, label in [('license', 'License'), ('notices', 'Notices')]:
            self.assertIn(f'Self::{label} => "{label}"', native)
            self.assertIn(f'data-info-page="{name}">{label}<', footer)
            self.assertIn(f'data-info-page="{name}">{label}<', tabs)
            self.assertIn(f'{name}: "', script)


class TermsVersion(unittest.TestCase):
    """TERMS.md is the one place the terms version is written."""

    def test_terms_version_matches_the_effective_date(self):
        text = (about.ROOT / 'TERMS.md').read_text(encoding='utf-8')
        version = about.terms_version(text)
        self.assertEqual(version, about.TERMS_VERSION)
        effective = re.search(r'^Effective (\w+ \d{1,2}, \d{4})', text, re.M).group(1)
        self.assertEqual(datetime.strptime(effective, '%B %d, %Y').date().isoformat(), version)

    def test_console_and_player_read_the_same_line(self):
        build = (about.ROOT / 'build.rs').read_text(encoding='utf-8')
        legal = (about.ROOT / 'src/legal.rs').read_text(encoding='utf-8')
        self.assertIn('"(terms version "', build)
        self.assertIn('DEFALT_TERMS_VERSION', build)
        self.assertIn('env!("DEFALT_TERMS_VERSION")', legal)
        self.assertIn('data-terms-version="{{TERMS_VERSION}}"',
                      (about.ROOT / 'web/index.html').read_text(encoding='utf-8'))

    def test_a_terms_file_without_a_version_is_refused(self):
        with self.assertRaises(ValueError):
            about.terms_version('# Terms\n\nEffective someday.')
