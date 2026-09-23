"""The fallback chain benches unhealthy models only, and retries bad drafts elsewhere."""
import json
import unittest
from unittest.mock import patch

import httpx

from radio import config, llm, session_backend

GOOD = json.dumps({"lines": [{"host": "mav", "text": "Fine."}, {"host": "rue", "text": "FINE."}]})


def reply(text, status=200):
    return httpx.Response(status, json={"choices": [{"message": {"content": text}}]})


class Chain(unittest.TestCase):
    def setUp(self):
        llm._COOLDOWN.clear()
        llm._NO_REASONING_FLAG.clear()
        llm._NO_JSON_FORMAT.clear()
        self.addCleanup(llm._COOLDOWN.clear)
        self.addCleanup(llm._NO_JSON_FORMAT.clear)
        self.addCleanup(llm._NO_REASONING_FLAG.clear)
        for p in (patch.object(config, "env", side_effect=lambda k, d="": {"OPENROUTER_API_KEY": "k"}.get(k, d)),
                  patch.object(llm, "_models", return_value=["first", "second"])):
            p.start()
            self.addCleanup(p.stop)

    def test_short_caller_deadline_timeout_does_not_bench_a_healthy_model(self):
        with patch.object(llm.httpx, "post", side_effect=httpx.ReadTimeout("slow")):
            self.assertIsNone(llm._openrouter_complete("s", "u", timeout=5))
        self.assertTrue(llm._available("first"))

    def test_a_long_timeout_does_bench(self):
        with patch.object(llm.httpx, "post", side_effect=httpx.ReadTimeout("slow")):
            llm._openrouter_complete("s", "u", timeout=40)
        self.assertFalse(llm._available("first"))

    def test_connect_errors_server_errors_and_rate_limits_bench(self):
        for response in (httpx.ConnectError("down"), httpx.Response(502), httpx.Response(429)):
            llm._COOLDOWN.clear()
            side = response if isinstance(response, Exception) else None
            with patch.object(llm.httpx, "post", side_effect=side, return_value=response):
                llm._openrouter_complete("s", "u", timeout=30)
            self.assertFalse(llm._available("first"))

    def test_plain_request_errors_do_not_bench(self):
        with patch.object(config, "env_bool", return_value=False), \
                patch.object(llm.httpx, "post", return_value=httpx.Response(404, text="no such model")):
            self.assertIsNone(llm._openrouter_complete("s", "u", timeout=30))
        self.assertTrue(llm._available("first"))

    def test_failed_validation_tries_the_next_model(self):
        with patch.object(llm.httpx, "post", side_effect=[reply('[{"host":"x"}]'), reply(GOOD)]) as post:
            result = llm._openrouter_complete("s", "u", timeout=30, json_mode=True,
                                              validator=lambda value: isinstance(value, dict))
        self.assertEqual(result, GOOD)
        self.assertEqual([c.kwargs["json"]["model"] for c in post.call_args_list], ["first", "second"])
        self.assertTrue(llm._available("first"))  # a bad draft is not an outage

    def test_json_object_mode_is_sent_and_dropped_when_rejected(self):
        responses = [httpx.Response(400, text="response_format is not supported"), reply(GOOD)]
        with patch.object(llm.httpx, "post", side_effect=responses) as post:
            self.assertEqual(llm._openrouter_complete("s", "u", timeout=30, json_object=True), GOOD)
        first, second = [c.kwargs["json"] for c in post.call_args_list]
        self.assertEqual(first["response_format"], {"type": "json_object"})
        self.assertNotIn("response_format", second)
        self.assertIn("first", llm._NO_JSON_FORMAT)
        with patch.object(llm.httpx, "post", return_value=reply(GOOD)) as post:
            llm._openrouter_complete("s", "u", timeout=30, json_object=True)
        self.assertNotIn("response_format", post.call_args.kwargs["json"])

    def test_json_object_is_opt_in(self):
        with patch.object(llm.httpx, "post", return_value=reply(GOOD)) as post:
            llm._openrouter_complete("s", "u", timeout=30)
        self.assertNotIn("response_format", post.call_args.kwargs["json"])

    def test_complete_json_forwards_validation_to_the_chain(self):
        with patch.object(session_backend, "enabled", return_value=False), \
                patch.object(llm, "_openrouter_complete", return_value=GOOD) as chain:
            llm.complete_json("s", "u", validator=bool, json_object=True)
        self.assertTrue(chain.call_args.kwargs["json_mode"])
        self.assertIs(chain.call_args.kwargs["validator"], bool)
        self.assertTrue(chain.call_args.kwargs["json_object"])


class CodexValidation(unittest.TestCase):
    def setUp(self):
        session_backend._RETRY_AT = 0
        session_backend._LANES.clear()
        self.addCleanup(session_backend._LANES.clear)

    def test_a_rejected_draft_does_not_bench_codex(self):
        def run(args, prompt, cwd, timeout):
            output = args[args.index("-o") + 1]
            with open(output, "w", encoding="utf-8") as handle:
                json.dump({"response": GOOD, "memory_refs": []}, handle)
            return "\n".join(json.dumps(e) for e in [
                {"type": "thread.started", "thread_id": "00000000-0000-0000-0000-000000000001"},
                {"type": "turn.completed"}])
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as folder, \
                patch.object(config, "CACHE_DIR", Path(folder)), \
                patch.object(session_backend, "executable", return_value="codex"), \
                patch.object(session_backend, "run_process", side_effect=run):
            result = session_backend.complete("s", "u", purpose="dialogue", memory=[],
                                              memory_fingerprint="none", timeout=5, json_mode=True,
                                              validator=lambda value: False)
            self.assertIsNone(result)
            self.assertEqual(session_backend._RETRY_AT, 0)
            self.assertEqual(session_backend.status()["last_result"]["provider"], "rejected")
            # The next request goes straight back to Codex.
            self.assertEqual(session_backend.complete("s", "u", purpose="utility", memory=[],
                                                      memory_fingerprint="none", timeout=5,
                                                      json_mode=True, validator=bool), GOOD)


if __name__ == "__main__":
    unittest.main()
