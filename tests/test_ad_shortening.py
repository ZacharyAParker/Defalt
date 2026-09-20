"""Oversized commissioned drafts get edited before they can reach speech."""
import unittest
from unittest.mock import patch

from radio import ad_copy
from radio.segments import base, writers


class AdShortening(unittest.TestCase):
    def test_unsponsored_disclaimer_is_not_a_stock_contrast_joke(self):
        self.assertTrue(base.valid_dialogue([dict(host='mav',text='This is not a real ad. Nobody paid us.')]))
        self.assertFalse(base.valid_dialogue([dict(host='mav',text="That's not a game, that's an invoice.")]))

    def test_contrast_filter_catches_curly_apostrophes_and_two_host_reframes(self):
        for text in ["That\u2019s not a game. That\u2019s an invoice.", "This isn't a game; it's an invoice.", "Not just a game but an invoice."]:
            self.assertFalse(base.valid_dialogue([dict(host='mav', text=text)]))
        self.assertFalse(base.valid_dialogue([dict(host='mav', text="That's not a game."),
                                             dict(host='rue', text="It's an invoice.")]))

    def test_keeps_long_draft_for_one_bounded_edit(self):
        long = [dict(host=host, text=('GabeCube wallet walnut. ' * 8).strip())
                for host in ('mav', 'rue', 'mav', 'rue')]
        short = [dict(host='mav', text='Steam Machine. Walnut optional. Financial recovery sold separately.'),
                 dict(host='rue', text='Look at your cart like it personally wronged you.')]
        with patch.object(base.llm, 'complete_json', side_effect=[long, short]) as model:
            result = base.write('An ad, about 22 seconds.', fallback=[], word_limit=51, repair_budget=True)
        self.assertEqual([line.text for line in result], [line['text'] for line in short])
        self.assertEqual(model.call_count, 2)
        first, second = model.call_args_list
        self.assertTrue(first.kwargs['validator'](long))
        self.assertFalse(second.kwargs['validator'](long))
        self.assertIn('GabeCube wallet walnut.', second.args[1])
        self.assertLessEqual(second.kwargs['timeout'], 45)

    def test_empty_attempt_gets_one_concise_retry_without_stock_substitution(self):
        short = [dict(host='mav', text='Walnut optional.'), dict(host='rue', text='Rent apparently optional too.')]
        with patch.object(base.llm, 'complete_json', side_effect=[None, short]) as model:
            self.assertEqual(len(base.write('An ad.', fallback=[], word_limit=51, repair_budget=True)), 2)
            self.assertEqual(model.call_count, 2)

    def test_second_overlong_draft_keeps_whole_setup_and_reply(self):
        draft = [dict(host='mav',text='Steam Machine. Walnut optional.'),
                 dict(host='rue',text='More details. ' * 20),
                 dict(host='mav',text='Even more details. ' * 14),
                 dict(host='rue',text='Look at your cart like it personally wronged you.')]
        with patch.object(base.llm, 'complete_json', side_effect=[draft,draft]):
            result = base.write('A short ad.',fallback=[],word_limit=20,repair_budget=True)
        self.assertEqual([line.text for line in result],[draft[0]['text'],draft[3]['text']])
        with patch.object(base.llm, 'complete_json', return_value=None) as model:
            self.assertEqual(base.write('An ad.', fallback=[], word_limit=51, repair_budget=True), [])
            self.assertEqual(model.call_count, 2)

    def test_ad_reserves_disclaimer_inside_its_total_budget(self):
        settings = {'ads.target_seconds': 22, 'ads.require_disclaimer': True}
        lines = [base.Line('mav', 'A ' * 25), base.Line('rue', 'B ' * 26)]
        with patch.object(writers.config.games, 'get', side_effect=lambda k,d=None:settings.get(k,d)), \
             patch.object(ad_copy, 'plan', return_value={'style':'dry','angle':'a pitch','history':[]}), \
             patch.object(ad_copy, 'finish', side_effect=lambda s,p,lines,*a,**kw:lines), \
             patch.object(writers, 'write', return_value=lines) as write:
            result = writers.game_ad({'ad_brief':'Shorten the supplied Steam Machine jokes.'})
        self.assertEqual(write.call_args.kwargs['word_limit'], 51)
        self.assertTrue(write.call_args.kwargs['repair_budget'])
        self.assertEqual(sum(len(line.text.split()) for line in result), 57)
        self.assertIn('Unsponsored', result[-1].text)

    def test_failed_generation_is_not_misreported_as_proven_overlength(self):
        with patch.object(ad_copy, 'plan', return_value={'style':'dry','angle':'a pitch','history':[]}), \
             patch.object(writers, 'write', return_value=[]):
            with self.assertRaisesRegex(ValueError, 'no usable short script'):
                writers.game_ad({'ad_brief':'Keep the original jokes, but shorter.'})

    def test_backup_sketch_rotates_even_when_the_product_name_changes(self):
        history=[]
        payoffs=[]
        for name in ['Steam Machine','Dale & Dawson Stationery Supplies','Other Game','Another Game','Game Five','Game Six']:
            lines=ad_copy.fallback({'name':name},history)
            payoff=ad_copy.normalized(lines[1:3])
            self.assertNotIn(payoff,payoffs)
            payoffs.append(payoff)
            history.insert(0,{'product':name,'lines':lines})
        changed=list(history[0]['lines'])
        changed[0]='A completely different product. My presentation has a slide deck.'
        self.assertTrue(ad_copy.repeated(changed,history))
