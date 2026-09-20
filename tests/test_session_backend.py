"""Session isolation, memory boundaries, and provider failover."""
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch
import uuid

from radio import config, director_memory, llm, session_backend as backend
from radio.segments.base import valid_dialogue

LINES = [{'host': 'mav', 'text': 'We chose that one.'},
         {'host': 'rue', 'text': 'I stand by it.'}]


class SessionBackendTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.thread = str(uuid.uuid4())
        self.calls = []
        self.settings = {'provider': 'codex', 'memory_vault': str(self.root / 'vault')}
        backend._LANES.clear()
        backend._LAST.clear()
        backend._RETRY_AT = 0
        self.patches = [patch.object(backend, 'setting', side_effect=lambda k,d=None:self.settings.get(k,d)),
                        patch.object(backend, 'executable', return_value='codex.exe'),
                        patch.object(config, 'CACHE_DIR', self.root / 'cache')]
        for p in self.patches:
            p.start()
        (self.root / 'vault/Notes').mkdir(parents=True)
        self.note = self.root / 'vault/Notes/Listener.md'
        self.note.write_text('''---
id: music
status: active
broadcast: true
basis: direct
core: true
sources: [private-source]
keywords: [music]
---
## Facts
- Prefer original recordings.
## Sources
PRIVATE_NOT_FOR_MODEL
''')

    def tearDown(self):
        for p in reversed(self.patches):
            p.stop()
        backend._LANES.clear()
        backend._LAST.clear()
        backend._RETRY_AT = 0
        self.temp.cleanup()

    def transport(self, args, prompt, cwd, timeout):
        self.calls.append((args, json.loads(prompt)))
        Path(args[args.index('-o') + 1]).write_text(json.dumps(
            {'response': json.dumps(LINES), 'memory_refs': ['music']}))
        return '\n'.join(json.dumps(e) for e in [
            {'type': 'thread.started', 'thread_id': self.thread},
            {'type': 'turn.started'}, {'type': 'turn.completed'}])

    def call(self, purpose='dialogue'):
        memory, fingerprint, _ = backend.prepare('Music break', purpose)
        return backend.complete('Write dialogue', 'Music break', purpose=purpose,
            memory=memory, memory_fingerprint=fingerprint, timeout=5,
            json_mode=True, validator=valid_dialogue)

    def test_live_dialogue_boundary_uses_and_resumes_owned_thread(self):
        with patch.object(backend, 'run_process', side_effect=self.transport), patch.object(llm, '_openrouter_complete') as fallback:
            for _ in range(2):
                result = llm.complete_json('Write dialogue', 'Music break',
                    purpose='dialogue', validator=valid_dialogue)
                self.assertEqual(result, LINES)
        fallback.assert_not_called()
        self.assertIn(self.thread, self.calls[1][0])
        self.assertIn('resume', self.calls[1][0])
        self.assertNotIn('--last', self.calls[1][0])
        self.assertNotIn('PRIVATE_NOT_FOR_MODEL', json.dumps(self.calls))
        self.assertNotIn('private-source', json.dumps(self.calls))

    def test_memory_change_starts_fresh_conversation(self):
        with patch.object(backend, 'run_process', side_effect=self.transport):
            self.call()
            self.note.write_text(self.note.read_text().replace('original','studio'))
            self.call()
        self.assertNotIn('resume', self.calls[1][0])

    def test_skip_privacy_change_discards_previous_dialogue_session(self):
        with patch.object(backend, 'run_process', side_effect=self.transport):
            with patch.object(config.station, 'get', return_value=False):
                self.call()
            with patch.object(config.station, 'get', return_value=True):
                self.call()
        self.assertNotIn('resume', self.calls[1][0])

    def test_revocation_removes_facts_and_resets_session(self):
        with patch.object(backend, 'run_process', side_effect=self.transport):
            self.call()
            self.note.write_text(self.note.read_text().replace('broadcast: true','broadcast: false'))
            memory, fingerprint, error = backend.prepare('music','dialogue')
        self.assertEqual(memory, [])
        self.assertIsNone(error)
        self.assertNotEqual(fingerprint, backend._LANES['dialogue']['state']['fingerprint'])

    def test_missing_vault_does_not_reuse_old_facts(self):
        self.settings['memory_vault'] = str(self.root / 'missing')
        memory, fingerprint, error = backend.prepare('music','dialogue')
        self.assertEqual(memory, [])
        self.assertEqual(fingerprint,'invalid-memory')
        self.assertTrue(error)

    def test_utilities_do_not_get_personal_memory(self):
        self.assertEqual(backend.prepare('music', 'utility'), ([], 'no-memory', None))
        args = backend.command('codex',self.root,self.root/'out',None,'gpt-5.6-luna','medium',False)
        self.assertIn('--ephemeral',args)

    def test_provider_off_keeps_existing_route(self):
        self.settings['provider'] = 'openrouter'
        with patch.object(backend,'complete') as session, patch.object(llm,'_openrouter_complete',return_value='ready'):
            self.assertEqual(llm.complete('test','test'),'ready')
            session.assert_not_called()

    def test_failure_uses_openrouter_with_same_approved_facts(self):
        with patch.object(backend,'run_process',side_effect=backend.SessionUnavailable('deadline')), patch.object(llm,'_openrouter_complete',return_value=json.dumps(LINES)) as fallback:
            result=llm.complete_json('Write dialogue','music',purpose='dialogue',validator=valid_dialogue)
        self.assertEqual(result,LINES)
        self.assertIn('Prefer original recordings',fallback.call_args.args[0])
        self.assertNotIn('return your answer in response',fallback.call_args.args[0])
        self.assertEqual(backend.status()['last_result']['provider'],'openrouter')
        self.assertEqual(backend._LANES['dialogue']['state'],{})

    def test_bad_json_or_unknown_reference_falls_back(self):
        def bad(*args):
            result=self.transport(*args)
            command=args[0]
            Path(command[command.index('-o')+1]).write_text(json.dumps({'response':'not json','memory_refs':['invented']}))
            return result
        with patch.object(backend,'run_process',side_effect=bad):
            self.assertIsNone(self.call())
        self.assertEqual(backend._LANES['dialogue']['state'],{})

    def test_tool_events_reject_the_turn(self):
        def bad(*args):
            return self.transport(*args)+'\n'+json.dumps({'type':'item.completed','item':{'type':'command_execution'}})
        with patch.object(backend,'run_process',side_effect=bad):
            self.assertIsNone(self.call())

    def test_plain_prose_gets_one_fresh_format_retry_with_shared_deadline(self):
        remaining=[]
        def transport(*args):
            remaining.append(args[3])
            result=self.transport(*args)
            if len(self.calls)==1:
                Path(args[0][args[0].index('-o')+1]).write_text(json.dumps({'response':'Sure, doing that now.','memory_refs':['music']}))
            return result
        with patch.object(backend,'run_process',side_effect=transport):
            self.assertEqual(json.loads(self.call()),LINES)
        self.assertEqual(len(self.calls),2)
        self.assertNotIn('resume',self.calls[1][0])
        self.assertIn('format_reminder',self.calls[1][1])
        self.assertLessEqual(remaining[1],remaining[0])
        self.assertEqual(backend.status()['last_result']['format_retries'],1)

    def test_format_retry_is_bounded_and_still_rejects_prose(self):
        def transport(*args):
            result=self.transport(*args)
            Path(args[0][args[0].index('-o')+1]).write_text(json.dumps({'response':'Still prose.','memory_refs':['music']}))
            return result
        with patch.object(backend,'run_process',side_effect=transport):
            self.assertIsNone(self.call())
        self.assertEqual(len(self.calls),2)
        self.assertEqual(backend._LANES['dialogue']['state'],{})

    def test_busy_lane_falls_back_without_waiting(self):
        lock=threading.Lock()
        lock.acquire()
        backend._LANES['dialogue']={'lock':lock,'state':{}}
        try:
            with patch.object(backend,'run_process') as process:
                self.assertIsNone(self.call())
                process.assert_not_called()
        finally:
            lock.release()

    def test_spawn_does_not_inherit_app_keys_or_parent_thread(self):
        with patch.dict(os.environ,{'OPENROUTER_API_KEY':'secret','CODEX_THREAD_ID':'unrelated'}):
            env=backend.child_environment()
            self.assertNotIn('OPENROUTER_API_KEY',env)
            self.assertNotIn('CODEX_THREAD_ID',env)
        args=backend.command('codex',self.root,self.root/'out',None,'gpt-5.6-luna','medium',True)
        for arg in ['--ignore-user-config','--ignore-rules','sandbox_mode="read-only"','features.shell_tool=false','features.hooks=false']:
            self.assertIn(arg,args)

    def test_memory_symlink_escape_is_rejected(self):
        outside=self.root/'outside.md'
        outside.write_text(self.note.read_text())
        try:
            (self.note.parent/'escape.md').symlink_to(outside)
        except OSError:
            self.skipTest('OS cannot create symlinks')
        with self.assertRaises(director_memory.MemoryUnavailable):
            director_memory.load_memory(self.root/'vault')

    def test_exhausted_deadline_never_calls_fallback(self):
        with patch.object(backend,'complete',return_value=None), patch.object(llm,'_openrouter_complete') as fallback:
            self.assertIsNone(llm.complete('test','test',timeout=0))
            fallback.assert_not_called()

    def test_invalid_dialogue_is_not_sent_to_speech(self):
        for result in [None,[],[{'host':'unknown','text':'hello'}],
                       [{'host':'mav','text':"That's not music, that's a problem."}],
                       [{'host':'mav','text':'word '*46}]]:
            self.assertFalse(valid_dialogue(result))

    def test_spoken_budget_is_enforced_across_both_hosts(self):
        from radio.segments import base
        with patch.object(llm, 'complete_json', return_value=LINES) as completion:
            base.write('Two lines, about 12 seconds total.', fallback=[])
        self.assertIn('31 spoken words TOTAL', completion.call_args.args[1])
        validator = completion.call_args.kwargs['validator']
        self.assertTrue(validator(LINES))
        self.assertFalse(validator([{'host':'mav','text':'word '*20},
                                    {'host':'rue','text':'word '*20}]))


if __name__ == '__main__':
    unittest.main()
