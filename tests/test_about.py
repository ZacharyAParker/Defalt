"""Release information must be available without starting playback or reading private data."""
import unittest
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
            self.assertEqual(set(data), {'version', 'copyright', 'documents'})
            self.assertEqual(set(data['documents']), {'patches', 'privacy', 'terms', 'copyright'})
            for name, text in data['documents'].items():
                self.assertEqual(text, (about.ROOT / about.DOCUMENTS[name]).read_text(encoding='utf-8'))
                self.assertIn(f'Defalt v{data["version"]}', text)
                self.assertIn(data['copyright'], text)
            page = client.get('/')
            self.assertIn(f'Defalt v{data["version"]}', page.get_data(as_text=True))
            self.assertNotIn('{{APP_VERSION}}', page.get_data(as_text=True))
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
