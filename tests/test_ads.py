import random
import threading
import unittest
from unittest.mock import patch, Mock

from radio import ads, config, director, timeline
from radio.app import app
from radio.segments import writers
from radio.segments.base import Line
from radio.sources import steam
from tests.test_transitions import value_at


class AdScheduling(unittest.TestCase):
    def setUp(self):
        self.s = director.Station.__new__(director.Station)
        self.s.lock = threading.RLock()
        self.s.clock = Mock(running=True)
        self.s.clock.now.return_value = 20.
        self.s.rng = random.Random(1)
        self.s.schedule = timeline.Schedule()
        self.s._stop = threading.Event()
        self.s._last_track = None
        self.ads = ads.for_station(self.s)
        self.settings = {'ducking.target_gain': .1, 'transitions.smart_cues': False,
                         'transitions.tempo_match': False}
        self.patches = [patch.object(config.station, 'get', side_effect=lambda k,d=None: self.settings.get(k,d)),
                        patch.object(config.games, 'get', side_effect=lambda k,d=None: d),
                        patch.object(ads.db, 'mark_aired')]
        for p in self.patches: p.start(); self.addCleanup(p.stop)
        self.music = self.s.schedule.add_music('song', dict(key='a', title='A', artist='A', duration=180))

    def ready(self, timing):
        self.ads.request = dict(id='ad-test', timing=timing, state='ready', product='Queue Insurance')
        self.ads.voices = [timeline.VoiceLine(url='voice', duration=8, host='mav', text='An ad.')]

    def test_now_keeps_music_timing_and_ducks_it_without_an_epoch_jump(self):
        before = self.music.start_at, self.music.offset, self.music.duration, self.music.id
        self.ready('now')
        self.ads.tick()
        self.assertEqual(self.ads.request['start_at'], 25.)
        self.assertEqual(before, (self.music.start_at, self.music.offset, self.music.duration, self.music.id))
        self.assertLessEqual(value_at(self.music.envelope, 27-self.music.start_at), .101)
        self.assertEqual(self.s.schedule.items[-1].meta['segment'], 'Ad break')
        self.assertFalse(hasattr(self.s, '_epoch'))

    def test_next_break_joins_existing_group_without_moving_or_overlapping_hosts(self):
        a = self.s.schedule.add_voice('a', 80, 8)
        b = self.s.schedule.add_voice('b', 86, 8)
        self.ready('next_break')
        self.ads.tick()
        self.assertAlmostEqual(self.ads.request['start_at'], 94.6)
        self.assertEqual((a.start_at,b.start_at), (80,86))

    def test_waits_for_a_break_that_has_not_been_built_yet(self):
        self.ready('next_break')
        self.ads.tick()
        self.assertEqual(self.ads.request['state'], 'ready')
        self.s.schedule.add_voice('future', 60, 10)
        self.ads.tick()
        self.assertEqual(self.ads.request['state'], 'scheduled')

    def test_now_waits_out_overlapping_host_lines_and_no_duplicate_insertion(self):
        self.s.schedule.add_voice('host', 18, 12)
        self.s.schedule.add_voice('other-host', 28, 8)
        self.ready('now')
        self.ads.tick()
        self.assertAlmostEqual(self.ads.request['start_at'], 36.6)
        before = len(self.s.schedule.items)
        self.ads.tick()
        self.assertEqual(len(self.s.schedule.items),before)

    def test_both_crossfading_decks_duck(self):
        b = self.s.schedule.add_music('b',dict(key='b', title='B', artist='B', duration=180))
        self.s.clock.now.return_value = b.start_at - 5
        self.ready('now')
        self.ads.tick()
        for music in (self.music,b):
            self.assertLessEqual(value_at(music.envelope,b.start_at+2-music.start_at),.101)

    def test_repeated_click_reuses_preparation_and_rejects_off_air(self):
        with patch.object(ads.threading.Thread,'start') as start:
            first=self.ads.queue('next_break')
            second=self.ads.queue('now')
            self.assertEqual(first['id'],second['id'])
            start.assert_called_once()
        self.s.clock.running=False
        with self.assertRaises(ValueError): self.ads.queue('now')

    def test_failed_synthesis_does_not_claim_a_scheduled_ad(self):
        self.ads.request=dict(id='test',state='preparing',timing='now')
        self.s._render=Mock(return_value=[])
        with patch.object(writers,'game_ad',return_value=[Line('mav','Ad')]):
            self.ads._prepare('test')
        self.assertEqual(self.ads.public()['state'],'failed')
        self.assertFalse(self.ads.public()['busy'])

    def test_custom_brief_is_passed_to_writer_and_conflicting_request_is_not_dropped(self):
        news={'gaming':{'enabled':True,'feeds':['https://example.test/rss']}}
        with patch.object(config.news,'get',return_value=news),patch.object(ads.threading.Thread,'start'):
            first=self.ads.queue('next_break',brief='Sarcastic gaming-news ad',news_category='gaming')
            self.assertEqual(first['brief'],'Sarcastic gaming-news ad')
            with self.assertRaisesRegex(ValueError,'not added'):
                self.ads.queue('next_break',brief='A different ad')
        self.s._render=Mock(return_value=[])
        with patch.object(writers,'game_ad',return_value=[]) as writer:
            self.ads._prepare(first['id'])
        self.assertEqual(writer.call_args.args[0]['ad_brief'],first['brief'])
        self.assertEqual(writer.call_args.args[0]['ad_news_category'],'gaming')

    def test_invalid_custom_ad_never_starts_a_worker(self):
        with patch.object(ads.threading.Thread,'start') as start,patch.object(config.news,'get',return_value={}):
            for kwargs in [{'brief':[]},{'brief':'x'*1201},{'brief':'News','news_category':[]},{'brief':'News','news_category':'unknown'}]:
                with self.assertRaises(ValueError):self.ads.queue('next_break',**kwargs)
            start.assert_not_called()

    def test_ad_brief_can_exceed_short_on_air_message_limit(self):
        brief='A sarcastic fictional product demo with a dry payoff. '*12
        with patch.object(ads.threading.Thread,'start'):
            result=self.ads.queue('next_break',brief=brief)
        self.assertEqual(result['brief'],brief.strip())

    def test_air_log_is_written_once_when_ad_reaches_playout(self):
        self.ready('now'); self.ads.tick()
        ads.db.mark_aired.assert_not_called()
        self.s.clock.now.return_value=26
        self.ads.tick(); self.ads.tick()
        ads.db.mark_aired.assert_called_once()
        self.s.clock.now.return_value=40
        self.assertEqual(self.ads.public()['state'],'done')

    def test_endpoint_validates_and_returns_preparation_without_waiting(self):
        with patch('radio.app.director.station',return_value=self.s), patch.object(ads.threading.Thread,'start'):
            with app.test_client() as client:
                for payload in [None,[],{}, {'timing':'yesterday'}, {'timing':[]}]:
                    self.assertEqual(client.post('/api/ads',json=payload).status_code,400)
                result=client.post('/api/ads',json={'timing':'now'})
                self.assertEqual(result.status_code,202)
                self.assertEqual(result.json['ad']['state'],'preparing')


class AdMaterial(unittest.TestCase):
    def setUp(self):
        # Existing copy checks must never write to the listener's station database.
        for mock in [patch.object(writers.ad_copy, 'recent', return_value=[]),
                     patch.object(writers.ad_copy, 'finish', side_effect=lambda s,p,lines,a,w: lines)]:
            mock.start()
            self.addCleanup(mock.stop)

    def test_missing_steam_uses_real_house_ad_material_instead_of_banter(self):
        with patch.object(steam,'wishlist',return_value=[]), patch.object(steam,'tracked_titles',return_value=[]), \
             patch.object(config.games,'get',side_effect=lambda k,d=None:d), patch.object(steam.db,'one',return_value=None):
            subject=steam.ad_subject()
        self.assertTrue(subject['fictional'])
        with patch.object(steam,'ad_subject',return_value=subject), patch.object(writers,'write',side_effect=lambda brief,**kw:kw['fallback']) as writer:
            lines=writers.game_ad({})
        self.assertEqual(len(lines),4)
        self.assertIn(subject['name'],lines[0].text)
        self.assertIn('POV sketch',writer.call_args.args[0])
        self.assertIn('explicitly fictional',writer.call_args.args[0])

    def test_disabled_ads_never_turn_into_banter(self):
        with patch.object(steam,'ad_subject',return_value=None), patch.object(writers,'banter') as banter:
            self.assertEqual(writers.game_ad({}),[])
            banter.assert_not_called()

    def test_long_generated_reads_use_short_product_specific_fallback(self):
        subject={'name':'Grass Touch Simulator','fictional':True}
        with patch.object(steam,'ad_subject',return_value=subject), \
             patch.object(writers,'write',return_value=[Line('rue','word '*200)]):
            lines=writers.game_ad({})
        self.assertLess(sum(len(x.text.split()) for x in lines),70)
        self.assertIn('graphics settings',lines[0].text)
        self.assertIn('No sponsors',lines[-1].text)

    def test_writer_cannot_omit_the_unsponsored_close(self):
        with patch.object(steam,'ad_subject',return_value={'name':'Test Game'}), \
             patch.object(writers,'write',return_value=[Line('rue','A brief joke.')]):
            lines=writers.game_ad({})
        self.assertIn('Nobody paid',lines[-1].text)

    def test_manual_non_steam_games_are_eligible(self):
        settings={'watchlist':[{'name':'Local Game','notes':'A puzzle game'}]}
        with patch.object(steam,'wishlist',return_value=[]), patch.object(steam,'tracked_titles',return_value=[]), \
             patch.object(config.games,'get',side_effect=lambda k,d=None:settings.get(k,d)):
            self.assertEqual(steam.ad_subject()['name'],'Local Game')

    def test_requested_news_ad_uses_supplied_sources_and_preserves_the_brief(self):
        story={'title':'A new handheld announced','summary':'SOURCE FACTS ONLY','source':'Example Games','published':123}
        context={'ad_brief':'Sarcastic gaming news with a dry payoff','ad_news_category':'gaming'}
        lines=[Line('rue','Our imaginary coping kit includes a tiny wallet.'),Line('mav','Unsponsored. Nobody paid for this.')]
        with patch.object(writers.sourceio,'_run',return_value=[story]) as source, \
             patch.object(steam,'ad_subject') as automatic, \
             patch.object(writers,'write',return_value=lines) as writer, \
             patch.object(writers.ad_copy,'finish',side_effect=lambda *a,**k:a[2]):
            self.assertEqual(writers.game_ad(context),lines)
        automatic.assert_not_called()
        source.assert_called_once_with('ad_news',{'category':'gaming'},30)
        self.assertIn(context['ad_brief'],writer.call_args.args[0])
        self.assertIn('SOURCE FACTS ONLY',writer.call_args.args[0])
        self.assertEqual(writer.call_args.kwargs['fallback'],[])

    def test_missing_news_and_failed_custom_copy_never_become_unrelated_stock_ads(self):
        context={'ad_brief':'Sarcastic gaming news','ad_news_category':'gaming'}
        for result in [[],None]:
            with patch.object(writers.sourceio,'_run',return_value=result),patch.object(writers,'write') as writer:
                with self.assertRaisesRegex(ValueError,'Nothing was scheduled'):writers.game_ad(context)
                writer.assert_not_called()
        with patch.object(writers,'write',return_value=[]):
            with self.assertRaisesRegex(ValueError,'Nothing was scheduled'):writers.game_ad({'ad_brief':'A very specific joke'})
