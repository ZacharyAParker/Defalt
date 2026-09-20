import tempfile
import time
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import Mock, patch

import httpx
from radio import config, trends


class TrendsTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(); self.addCleanup(tmp.cleanup)
        self.settings = {}
        for mock in (patch.object(config, 'CACHE_DIR', Path(tmp.name)), patch.object(trends, '_cache', None),
                     patch.object(trends.chart_scraper, 'fetch', return_value=[]),
                     patch.object(config.station, 'get', side_effect=lambda k,d=None:self.settings.get(k,d))):
            mock.start();self.addCleanup(mock.stop)

    def feed(self, age=0):
        return {'feed': {'updated':datetime.fromtimestamp(time.time()-age,timezone.utc).isoformat(),
                         'results':[{'artistName':'Example Artist','name':'Example Song',
                                     'genres':[{'name':'Pop'}]}]}}

    def test_dated_feed_preserves_actual_source_and_rank(self):
        result=trends.parse(self.feed(),'Apple Music','https://example.com','us')
        self.assertEqual(result[0]['rank'],1)
        self.assertEqual(result[0]['source'],'Apple Music')
        self.assertEqual(result[0]['genre'],'Pop')
        for bad in (self.feed(72*3600), {'feed':{'results':[]}}):
            with self.assertRaises(ValueError):trends.parse(bad,'Apple Music','url','us')

    def test_itunes_fallback_does_not_claim_apple_streaming_or_spotify_rank(self):
        feed={'feed':{'updated':{'label':self.feed()['feed']['updated']},'entry':[
            {'im:artist':{'label':'Example Artist'},'im:name':{'label':'Example Song'}}]}}
        response=Mock();response.json.return_value=feed
        with patch.object(trends.httpx,'get',side_effect=[httpx.ReadTimeout('offline'),response]):
            self.assertEqual(trends.refresh()['source'],'iTunes')
        self.assertEqual(trends.cached()[0]['source'],'iTunes')

    def test_selection_reads_cache_without_network_and_stale_data_loses_boost(self):
        response=Mock();response.json.return_value=self.feed()
        with patch.object(trends.httpx,'get',return_value=response) as get:
            trends.refresh();trends.refresh()
            self.assertEqual(get.call_count,1)
        tracks=[(1,{'artist':'Example Artist','title':'Example Song','selection':{}}),
                (1,{'artist':'Other','title':'Example Song','selection':{}})]
        with patch.object(trends.httpx,'get',side_effect=AssertionError('No network during selection')):
            weights=trends.influence(tracks)
        self.assertGreater(weights[0][0],weights[1][0])
        self.assertEqual(weights[0][1]['selection']['trend']['source'],'Apple Music')
        with patch.object(trends.time,'time',return_value=time.time()+72*3600):
            self.assertEqual(trends.influence(tracks),tracks)

    def test_disabled_does_not_fetch_or_promote(self):
        self.settings['selection.trends_enabled']=False
        with patch.object(trends.httpx,'get') as get:
            self.assertEqual(trends.refresh()['state'],'disabled')
            self.assertEqual(trends.cached(),[])
        get.assert_not_called()

    def test_failure_backoff_and_corrupt_cache_do_not_break_selection(self):
        with patch.object(trends.httpx,'get',side_effect=httpx.ReadTimeout('offline')) as get:
            self.assertEqual(trends.refresh()['state'],'unavailable')
            trends.refresh()
            self.assertEqual(get.call_count,2)
        trends._cache['items']=[{}, {'rank':1,'published_at':'bad'}]
        self.assertEqual(trends.cached(),[])

    def test_sources_keep_independent_refresh_clocks_and_freshness(self):
        apple=trends.parse(self.feed(),'Apple Music','apple','us')
        spotify=[{**apple[0], 'source':'Spotify via Kworb', 'rank':5,
                  'published_at':time.time()-60*3600}]
        with patch.object(trends,'_apple',return_value=apple) as get_apple, \
             patch.object(trends.chart_scraper,'fetch',return_value=spotify) as scrape:
            self.assertEqual(trends.refresh()['sources'],['Apple Music','Spotify via Kworb'])
            trends.refresh()
            self.assertEqual((get_apple.call_count,scrape.call_count),(1,1))
            with patch.object(trends.time,'time',return_value=time.time()+3*3600):
                trends.refresh()
                self.assertEqual((get_apple.call_count,scrape.call_count),(2,1))
        self.assertEqual(len(trends.cached()),2)
        with patch.object(trends.time,'time',return_value=time.time()+13*3600):
            self.assertEqual([x['source'] for x in trends.cached()],['Apple Music'])
        self.assertEqual(trends.match(apple[0], spotify+apple)['source'],'Apple Music')

    def test_scraper_failure_preserves_fresh_cache_and_respects_backoff(self):
        now=time.time()
        item=trends.parse(self.feed(),'Spotify via Kworb','kworb','us')
        with patch.object(trends,'_apple',return_value=[]), \
             patch.object(trends.chart_scraper,'fetch',return_value=item) as scrape:
            trends.refresh()
            scrape.side_effect=httpx.ReadTimeout('offline')
            with patch.object(trends.time,'time',return_value=now+7*3600):
                trends.refresh();trends.refresh()
                self.assertEqual(scrape.call_count,2)
                self.assertEqual(len(trends.cached()),1)
