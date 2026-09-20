import time
import unittest
from datetime import datetime, timezone
from unittest.mock import Mock, patch

import httpx
from radio import chart_scraper as scraper


class ChartScraperTests(unittest.TestCase):
    def html(self, age=0):
        date=datetime.fromtimestamp(time.time()-age,timezone.utc).strftime('%Y/%m/%d')
        return f'''<span class="pagetitle">Spotify Daily Chart - United States - {date} | Totals</span>
        <table id="spotifydaily"><thead><tr><th>Pos</th></tr></thead><tbody>
        <tr><td>1</td><td>+2</td><td><div>
        <a href="../artist/{'a'*22}.html">Artist &amp; Friend</a> -
        <a href="../track/{'b'*22}.html">Song - Part Two</a>
        (w/ <a href="../artist/{'c'*22}.html">Guest</a>)</div></td><td>4</td></tr>
        </tbody></table>'''

    def test_parses_links_not_hyphens_or_guest_names(self):
        item=scraper.parse(self.html(),'us','source-url')[0]
        self.assertEqual((item['artist'],item['title']),('Artist & Friend','Song - Part Two'))
        self.assertEqual(item['source'],'Spotify via Kworb')
        self.assertEqual(item['spotify_id'],'b'*22)
        self.assertEqual(item['url'],'source-url')
        self.assertEqual(item['rank'],1)

    def test_rejects_stale_wrong_region_and_changed_layout(self):
        for html,region in [(self.html(96*3600),'us'),(self.html(),'gb'),
                            (self.html().replace('spotifydaily','other'),'us'),
                            (self.html().replace('pagetitle','other'),'us'),
                            (self.html(),'zz'), ('Log in to continue','us')]:
            with self.subTest(region=region,html=html[:50]),self.assertRaises(ValueError):
                scraper.parse(html,region,'url')

    def test_fetch_checks_robots_before_chart_and_never_uses_login(self):
        robots=Mock(text='User-agent: *\nAllow: /')
        chart=Mock(text=self.html())
        client=Mock();client.get.side_effect=[robots,chart]
        with patch.object(scraper.httpx,'Client') as factory:
            factory.return_value.__enter__.return_value=client
            result=scraper.fetch('us')
        self.assertEqual(len(result),1)
        self.assertEqual([call.args[0] for call in client.get.call_args_list],
                         ['https://kworb.net/robots.txt','https://kworb.net/spotify/country/us_daily.html'])
        self.assertNotIn('cookies',factory.call_args.kwargs)

    def test_disallowed_or_failed_robots_does_not_request_chart(self):
        for fail in (False,True):
            client=Mock();response=Mock(text='User-agent: *\nDisallow: /spotify/')
            client.get.return_value=response
            if fail:
                client.get.side_effect=httpx.ReadTimeout('offline')
            with patch.object(scraper.httpx,'Client') as factory:
                factory.return_value.__enter__.return_value=client
                if fail:
                    with self.assertRaises(httpx.ReadTimeout):scraper.fetch('us')
                else:
                    self.assertEqual(scraper.fetch('us'),[])
            self.assertEqual(client.get.call_count,1)
