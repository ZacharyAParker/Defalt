import unittest
from unittest.mock import patch

from radio.segments import news_context, writers
from radio.segments.base import Line


DETAIL = ('The observatory opened its new visitor centre on Monday after a two year renovation funded by local donations. '
          'Visitors can book free evening tours every Friday, and the first programme includes demonstrations of the telescope and talks by the staff.')


class NewsContextTests(unittest.TestCase):
    def setUp(self):
        news_context._CACHE.clear()
        self.story = {'title':'Observatory opens visitor centre','source':'Local Bulletin','summary':DETAIL,'ident':'test-story'}

    def test_model_failure_reads_source_details_instead_of_headline_filler(self):
        context = {'speech_budget':28}
        with patch.object(writers.rss,'stories',return_value=('Science',[self.story])), patch('radio.segments.base.llm.complete_json',return_value=None):
            lines = writers.news(context)
        self.assertEqual(len(lines),1)
        # Headline plus one complete sentence: a short bulletin, not a recitation.
        self.assertIn('Monday',lines[0].text)
        self.assertIn('Observatory opens visitor centre',lines[0].text)
        self.assertNotIn('Friday',lines[0].text)
        self.assertLessEqual(len(lines[0].text.split()),news_context.FALLBACK_WORDS)
        self.assertIn('Local Bulletin',lines[0].text)
        self.assertNotIn('whole story',lines[0].text)
        self.assertEqual(context['_news_items'],[self.story])

    def test_thin_feed_uses_bounded_article_fetch_and_caches_it(self):
        thin = {**self.story,'summary':'A centre opened.','link':'https://example.com/news'}
        with patch.object(news_context.sourceio,'_run',return_value={'text':DETAIL}) as fetch:
            for _ in range(2):
                self.assertEqual(news_context.prepare([thin])[0]['summary'],DETAIL)
        fetch.assert_called_once_with('article',{'url':thin['link']},8)

    def test_unavailable_thin_article_is_silent_not_fake_news_and_not_consumed(self):
        thin = {**self.story,'summary':'Anthropic is operating a lab that conducts biology experiments.','link':'https://example.com/thin'}
        with patch.object(writers.rss,'stories',return_value=('Tech',[thin])), patch.object(news_context.sourceio,'_run',side_effect=TimeoutError), patch.object(writers.rss,'mark_read') as mark, patch.object(writers,'write') as model:
            self.assertEqual(writers.compose('news',{}),[])
            self.assertEqual(writers.compose('news',{}),[])
        mark.assert_not_called()
        model.assert_not_called()

    def test_generated_headline_only_exchange_is_replaced(self):
        bad = [Line('mav','The observatory opened.'),Line('rue',"that's it? that's the whole story?"),Line('mav','That is the whole story.')]
        with patch.object(writers.rss,'stories',return_value=('Science',[self.story])), patch.object(writers,'write',return_value=bad):
            lines=writers.news({})
        self.assertIn('Monday',lines[0].text)
        self.assertNotIn('whole story',' '.join(l.text for l in lines))

    def test_substantive_feed_needs_no_article_request(self):
        with patch.object(news_context.sourceio,'_run') as fetch:
            self.assertEqual(news_context.prepare([self.story]),[self.story])
        fetch.assert_not_called()

    def test_ad_sources_exclude_stale_undated_and_future_reports(self):
        now=1_000_000
        stories=[{**self.story,'published':stamp} for stamp in [0,now+60,now-200_000,now-300,now-60]]
        with patch.object(writers.rss,'stories',return_value=('Gaming',stories)), \
             patch.object(news_context.time,'time',return_value=now), \
             patch.object(news_context.config.news,'get',return_value=30):
            result=news_context.for_ad('gaming')
        self.assertEqual([s['published'] for s in result],[now-60,now-300])

    def test_fallback_never_cuts_a_sentence_or_exceeds_budget(self):
        lines=news_context.fallback(self.story,'mav',28)
        self.assertTrue(lines[0].text.endswith('.'))
        self.assertLessEqual(len(lines[0].text.split()),int(28*2.3))
        self.assertEqual(news_context.fallback({**self.story,'summary':'One enormous sentence ' * 90 + '.'},'mav'),[])
