"""Remote access: Cloudflare Access tokens, the host allowlist, /listen and the tunnel."""
import base64
import hashlib
import json
import os
import random
import socket
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from radio import remote_api, remote_auth, remote_tunnel
from radio.app import app
from tests.test_http_infra import FakeStation

TEAM = "team.cloudflareaccess.com"
AUD = "a" * 64
HOST = "radio.example.org"
LOCAL = "http://127.0.0.1:8090"


# --------------------------------------------------------------------------
# A real RSA key, made here: there is no crypto library to lean on.
# --------------------------------------------------------------------------
def _probable_prime(bits, rng):
    small = [p for p in range(3, 2000, 2) if all(p % q for q in range(3, int(p ** 0.5) + 1, 2))]
    while True:
        candidate = rng.getrandbits(bits) | (3 << (bits - 2)) | 1   # a full-width modulus
        if any(candidate % p == 0 for p in small):
            continue
        d, r = candidate - 1, 0
        while d % 2 == 0:
            d //= 2
            r += 1
        for _ in range(24):
            x = pow(rng.randrange(2, candidate - 2), d, candidate)
            if x in (1, candidate - 1):
                continue
            for _ in range(r - 1):
                x = pow(x, 2, candidate)
                if x == candidate - 1:
                    break
            else:
                break
        else:
            return candidate


def make_key(seed, bits=1024):
    rng = random.Random(seed)
    e = 65537
    while True:
        p, q = _probable_prime(bits // 2, rng), _probable_prime(bits // 2, rng)
        phi = (p - 1) * (q - 1)
        if p != q and phi % e:
            return p * q, e, pow(e, -1, phi)


def b64(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def int_b64(value):
    return b64(value.to_bytes((value.bit_length() + 7) // 8, "big"))


def sign(key, claims, kid="k1", alg="RS256"):
    n, _e, d = key
    head = b64(json.dumps({"alg": alg, "kid": kid, "typ": "JWT"}).encode())
    body = b64(json.dumps(claims).encode())
    size = (n.bit_length() + 7) // 8
    digest = remote_auth.SHA256_PREFIX + hashlib.sha256(f"{head}.{body}".encode()).digest()
    encoded = b"\x00\x01" + b"\xff" * (size - len(digest) - 3) + b"\x00" + digest
    signature = pow(int.from_bytes(encoded, "big"), d, n).to_bytes(size, "big")
    return f"{head}.{body}.{b64(signature)}"


def jwks(**keys):
    return {"keys": [{"kid": kid, "kty": "RSA", "alg": "RS256", "use": "sig",
                      "n": int_b64(key[0]), "e": int_b64(key[1])} for kid, key in keys.items()]}


KEY = make_key(1)
OTHER = make_key(2)


def claims(**changes):
    now = int(time.time())
    base = {"aud": [AUD], "iss": f"https://{TEAM}", "email": "zach@example.com",
            "exp": now + 600, "iat": now, "nbf": now - 5, "sub": "user"}
    base.update(changes)
    return {k: v for k, v in base.items() if v is not None}


class FakeCerts:
    def __init__(self, document):
        self.document = document
        self.urls = []

    def __call__(self, url):
        self.urls.append(url)
        if isinstance(self.document, Exception):
            raise self.document
        return self.document


class Clock:
    def __init__(self):
        self.value = 1000.0

    def __call__(self):
        return self.value


class TokenVerification(unittest.TestCase):
    def setUp(self):
        self.certs = FakeCerts(jwks(k1=KEY))
        self.clock = Clock()
        self.cache = remote_auth.KeyCache(fetch=self.certs, clock=self.clock)

    def verify(self, token, **kwargs):
        return remote_auth.verify(token, team=TEAM, aud=AUD, cache=self.cache, **kwargs)

    def test_a_valid_user_token(self):
        result = self.verify(sign(KEY, claims()))
        self.assertEqual(result["email"], "zach@example.com")
        self.assertEqual(self.certs.urls, [f"https://{TEAM}/cdn-cgi/access/certs"])
        self.assertEqual(remote_auth.identity(result), "zach@example.com")

    def test_a_service_token_is_accepted_by_its_common_name(self):
        result = self.verify(sign(KEY, claims(email=None, sub="", common_name="abc.access")))
        self.assertEqual(remote_auth.identity(result), "service:abc.access")

    def test_a_single_string_audience_counts(self):
        self.verify(sign(KEY, claims(aud=AUD)))

    def test_refusals(self):
        now = int(time.time())
        cases = {
            "wrong audience": sign(KEY, claims(aud=["b" * 64])),
            "wrong issuer": sign(KEY, claims(iss="https://evil.cloudflareaccess.com")),
            "expired": sign(KEY, claims(exp=now - 3600)),
            "not valid yet": sign(KEY, claims(nbf=now + 3600)),
            "no expiry": sign(KEY, claims(exp=None)),
            "bad signature": sign(OTHER, claims()),
            "no identity": sign(KEY, claims(email=None)),
            "unexpected algorithm": sign(KEY, claims(), alg="HS256"),
            "not a JWT": "abc.def",
            "garbage": "a.b.c",
        }
        for name, token in cases.items():
            with self.subTest(name):
                with self.assertRaises(remote_auth.Refused):
                    self.verify(token)

    def test_a_tampered_body_fails_the_signature(self):
        head, _body, signature = sign(KEY, claims()).split(".")
        forged = b64(json.dumps(claims(email="someone@else.com")).encode())
        with self.assertRaises(remote_auth.Refused):
            self.verify(f"{head}.{forged}.{signature}")

    def test_small_clock_skew_is_allowed(self):
        now = int(time.time())
        self.verify(sign(KEY, claims(exp=now - 20, nbf=now + 20)))

    def test_an_unknown_key_id_refetches_once_then_waits(self):
        self.verify(sign(KEY, claims()))
        self.certs.document = jwks(k1=KEY, k2=OTHER)
        self.verify(sign(OTHER, claims(), kid="k2"))   # rotated in: found after a refetch
        self.assertEqual(len(self.certs.urls), 2)
        for _ in range(3):
            with self.assertRaises(remote_auth.Refused):
                self.verify(sign(OTHER, claims(), kid="k9"))
        self.assertEqual(len(self.certs.urls), 2, "unknown kids must not hammer the certs endpoint")
        self.clock.value += remote_auth.UNKNOWN_KID_EVERY + 1
        with self.assertRaises(remote_auth.Refused):
            self.verify(sign(OTHER, claims(), kid="k9"))
        self.assertEqual(len(self.certs.urls), 3)

    def test_keys_are_cached_for_an_hour(self):
        for _ in range(5):
            self.verify(sign(KEY, claims()))
        self.assertEqual(len(self.certs.urls), 1)
        self.clock.value += remote_auth.KEYS_TTL + 1
        self.verify(sign(KEY, claims()))
        self.assertEqual(len(self.certs.urls), 2)

    def test_no_keys_means_no_entry(self):
        cache = remote_auth.KeyCache(fetch=FakeCerts(OSError("offline")), clock=self.clock)
        with self.assertRaises(remote_auth.Refused):
            remote_auth.verify(sign(KEY, claims()), team=TEAM, aud=AUD, cache=cache)

    def test_the_last_good_keys_survive_a_failed_refresh(self):
        self.verify(sign(KEY, claims()))
        self.certs.document = OSError("offline")
        self.clock.value += remote_auth.KEYS_TTL + 1
        self.verify(sign(KEY, claims()))

    def test_jwks_parsing_skips_what_it_cannot_use(self):
        document = jwks(k1=KEY)
        document["keys"].append({"kid": "ec", "kty": "EC"})
        document["keys"].append({"kid": "tiny", "kty": "RSA", "n": int_b64(99991), "e": "AQAB"})
        self.assertEqual(set(remote_auth.parse_jwks(document)), {"k1"})

    def test_rs256_rejects_a_short_signature(self):
        n, e, _ = KEY
        self.assertFalse(remote_auth.rs256_valid(b"x", b"\x01\x02", n, e))


ENV = {"REMOTE_HOSTS": HOST, "CF_ACCESS_TEAM_DOMAIN": TEAM, "CF_ACCESS_AUD": AUD}


class Guarded(unittest.TestCase):
    def setUp(self):
        self.station = FakeStation()
        patcher = patch("radio.app.director.station", return_value=self.station)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.env(ENV)
        cache = remote_auth.KeyCache(fetch=FakeCerts(jwks(k1=KEY)))
        patcher = patch.object(remote_auth, "keys", cache)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.client = app.test_client()

    def env(self, values):
        patcher = patch.dict(os.environ, values)
        patcher.start()
        self.addCleanup(patcher.stop)

    def get(self, host, token=None, path="/api/about", **headers):
        if token:
            headers["Cf-Access-Jwt-Assertion"] = token
        return self.client.get(path, base_url=LOCAL, headers={"Host": host, **headers})


class HostAllowlist(Guarded):
    def test_matrix(self):
        good = sign(KEY, claims())
        cases = [
            ("127.0.0.1:8090", None, {}, 200),                    # loopback unchanged
            ("localhost:8090", None, {}, 200),
            (HOST, good, {}, 200),                                # remote with a token
            (HOST.upper(), good, {}, 200),
            (f"{HOST}:443", good, {}, 200),
            (HOST, None, {}, 403),                                # remote, no token
            (HOST, sign(OTHER, claims()), {}, 403),               # remote, forged token
            (HOST, sign(KEY, claims(aud=["x"])), {}, 403),
            (f"{HOST}:8090", good, {}, 403),                      # an odd port is not the tunnel
            ("other.example.org", good, {}, 403),                 # not on the list, token or not
            (f"sub.{HOST}", good, {}, 403),
            ("127.0.0.1:8090", None, {"Cf-Ray": "abc"}, 403),     # through Cloudflare needs a token too
            ("127.0.0.1:8090", good, {"Cf-Connecting-Ip": "1.2.3.4"}, 200),
        ]
        for host, token, headers, status in cases:
            with self.subTest(host=host, token=bool(token), headers=headers):
                self.assertEqual(self.get(host, token, **headers).status_code, status)

    def test_the_cookie_works_when_the_header_is_missing(self):
        self.client.set_cookie("CF_Authorization", sign(KEY, claims()), domain=HOST)
        self.assertEqual(self.get(HOST).status_code, 200)
        self.client.set_cookie("CF_Authorization", sign(OTHER, claims()), domain=HOST)
        self.assertEqual(self.get(HOST).status_code, 403)

    def test_remote_hosts_without_access_are_refused(self):
        self.env({"CF_ACCESS_AUD": ""})
        self.assertEqual(self.get(HOST, sign(KEY, claims())).status_code, 403)
        self.assertEqual(self.get("127.0.0.1:8090").status_code, 200)

    def test_no_remote_hosts_means_loopback_only(self):
        self.env({"REMOTE_HOSTS": ""})
        self.assertEqual(self.get(HOST, sign(KEY, claims())).status_code, 403)

    def test_remote_writes_must_be_same_origin_https(self):
        token = sign(KEY, claims())
        for origin, status in ((f"https://{HOST}", 200), (f"http://{HOST}", 403),
                               ("https://evil.example", 403), (None, 200)):
            with self.subTest(origin=origin):
                headers = {"Host": HOST, "Cf-Access-Jwt-Assertion": token}
                if origin:
                    headers["Origin"] = origin
                response = self.client.post("/api/heartbeat", base_url=LOCAL, headers=headers)
                self.assertEqual(response.status_code, status)

    def test_a_remote_listener_cannot_shut_the_station_down(self):
        response = self.client.post("/api/shutdown", base_url=LOCAL,
                                    headers={"Host": HOST, "Cf-Access-Jwt-Assertion": sign(KEY, claims())})
        self.assertEqual(response.status_code, 403)
        self.assertEqual(self.station.shutdowns, 0)


class Upstream:
    """A stand-in for the console's stream server."""

    def __init__(self, chunks=(), status=200):
        outer = self
        self.requests = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802
                outer.requests.append((self.path, self.headers.get("Host")))
                if self.path == "/status":
                    body = json.dumps({"listeners": 1, "codec": "aac", "tunnel": {"state": "online"}}).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                self.send_response(status)
                self.send_header("Content-Type", "audio/aac")
                self.send_header("X-Burst-Seconds", "1.50")
                self.end_headers()
                for chunk in chunks:
                    self.wfile.write(chunk)
                    self.wfile.flush()
                    time.sleep(0.01)

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Listen(Guarded):
    def upstream(self, **kwargs):
        upstream = Upstream(**kwargs)
        self.addCleanup(upstream.close)
        self.env({"BROADCAST_PORT": str(upstream.port)})
        return upstream

    def test_the_stream_is_passed_through_as_it_comes(self):
        upstream = self.upstream(chunks=[b"\xff\xf1one", b"two", b"three"])
        response = self.get("127.0.0.1:8090", path="/listen")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.mimetype, "audio/aac")
        self.assertEqual(response.headers["Cache-Control"], "no-store")
        self.assertEqual(response.headers["X-Burst-Seconds"], "1.50")
        self.assertNotIn("Content-Length", response.headers)
        self.assertEqual(response.get_data(), b"\xff\xf1onetwothree")
        self.assertEqual(upstream.requests[0], ("/stream", f"127.0.0.1:{upstream.port}"))
        self.assertGreaterEqual(self.station.heartbeats, 1, "a stream listener must keep the clock running")

    def test_remote_listeners_need_access_too(self):
        self.upstream(chunks=[b"x"])
        self.assertEqual(self.get(HOST, path="/listen").status_code, 403)
        self.assertEqual(self.get(HOST, sign(KEY, claims()), path="/listen").status_code, 200)

    def test_no_console_is_a_friendly_503(self):
        self.env({"BROADCAST_PORT": str(free_port())})
        response = self.get("127.0.0.1:8090", path="/listen")
        self.assertEqual(response.status_code, 503)
        self.assertIn("start Defalt to listen", response.get_json()["error"])
        page = self.get("127.0.0.1:8090", path="/listen", Accept="text/html")
        self.assertEqual(page.status_code, 503)
        self.assertIn(b"isn't running at home", page.get_data())

    def test_an_upstream_error_is_the_same_503(self):
        self.upstream(status=503)
        self.assertEqual(self.get("127.0.0.1:8090", path="/listen").status_code, 503)


class RemoteStatus(Guarded):
    def test_with_the_console_running(self):
        self.env({"BROADCAST_PORT": str(self.upstreamed().port), "REMOTE_TUNNEL": "defalt"})
        body = self.get("127.0.0.1:8090", path="/api/remote/status").get_json()
        self.assertTrue(body["console_running"])
        self.assertEqual(body["broadcast"]["listeners"], 1)
        self.assertNotIn("tunnel", body["broadcast"])
        self.assertEqual(body["tunnel"]["state"], "online")
        self.assertTrue(body["access_configured"])
        self.assertEqual(body["remote_hosts"], [HOST])
        self.assertIn("enabled", body)
        self.assertIn("mute_local", body)

    def upstreamed(self):
        upstream = Upstream()
        self.addCleanup(upstream.close)
        return upstream

    def test_without_the_console(self):
        self.env({"BROADCAST_PORT": str(free_port())})
        body = self.get("127.0.0.1:8090", path="/api/remote/status").get_json()
        self.assertFalse(body["console_running"])
        self.assertIsNone(body["broadcast"])
        self.assertEqual(body["tunnel"]["state"], "off")

    def test_settings_are_saved_as_booleans_only(self):
        with patch.object(remote_api.config.station, "set_many") as saved:
            response = self.client.post("/api/remote/config", base_url=LOCAL, json={"mute_local": True})
            self.assertEqual(response.status_code, 200)
            saved.assert_called_once_with({"remote.mute_local": True})
            self.assertEqual(self.client.post("/api/remote/config", base_url=LOCAL,
                                              json={"enabled": "yes"}).status_code, 400)
            self.assertEqual(self.client.post("/api/remote/config", base_url=LOCAL, json={}).status_code, 400)


class AppShell(Guarded):
    def test_manifest_and_service_worker_are_served_from_the_root(self):
        manifest = self.get("127.0.0.1:8090", path="/manifest.webmanifest")
        self.assertEqual(manifest.status_code, 200)
        self.assertEqual(manifest.mimetype, "application/manifest+json")
        data = json.loads(manifest.get_data())
        self.assertEqual(data["name"], "Defalt")
        self.assertEqual(data["display"], "standalone")
        self.assertTrue(any("maskable" in icon.get("purpose", "") for icon in data["icons"]))
        for icon in data["icons"]:
            path = Path(__file__).resolve().parent.parent / "web" / icon["src"].lstrip("/")
            self.assertTrue(path.is_file(), icon["src"])
        worker = self.get("127.0.0.1:8090", path="/sw.js")
        self.assertEqual(worker.status_code, 200)
        self.assertEqual(worker.headers["Service-Worker-Allowed"], "/")
        self.assertNotIn(b"{{APP_VERSION}}", worker.get_data())

    def test_the_page_links_the_app_shell(self):
        page = self.get("127.0.0.1:8090", path="/").get_data(as_text=True)
        self.assertIn('rel="manifest"', page)
        self.assertIn('rel="apple-touch-icon"', page)
        self.assertIn('apple-mobile-web-app-capable', page)


class StandaloneTunnel(unittest.TestCase):
    ENV = {**ENV, "REMOTE_TUNNEL": "defalt", "USERPROFILE": r"C:\Users\someone"}

    def env(self, **changes):
        values = {**self.ENV, **changes}
        return lambda name: values.get(name, "")

    def test_command_and_default_paths(self):
        argv = remote_tunnel.tunnel_command(self.env(), exists=lambda p: True, which=lambda name: None)
        self.assertEqual(argv, [remote_tunnel.DEFAULT_BIN, "tunnel", "--config",
                                str(Path(r"C:\Users\someone") / ".cloudflared" / "defalt.yml"), "run", "defalt"])
        argv = remote_tunnel.tunnel_command(self.env(CLOUDFLARED_BIN=r"D:\cf.exe", CLOUDFLARED_CONFIG=r"D:\t.yml"),
                                            exists=lambda p: True, which=lambda name: r"C:\on\path.exe")
        self.assertEqual(argv[0], r"D:\cf.exe")
        self.assertEqual(argv[3], r"D:\t.yml")
        on_path = remote_tunnel.tunnel_command(self.env(), exists=lambda p: True, which=lambda name: r"C:\on\path.exe")
        self.assertEqual(on_path[0], r"C:\on\path.exe")

    def test_fails_closed(self):
        for missing in ("REMOTE_TUNNEL", "REMOTE_HOSTS", "CF_ACCESS_TEAM_DOMAIN", "CF_ACCESS_AUD"):
            with self.subTest(missing), self.assertRaises(remote_tunnel.NotConfigured):
                remote_tunnel.tunnel_command(self.env(**{missing: ""}), exists=lambda p: True)
        with self.assertRaises(remote_tunnel.NotConfigured):
            remote_tunnel.tunnel_command(self.env(), exists=lambda p: False)

    def test_connections_follow_the_log(self):
        count = 0
        for line in ("INF Starting tunnel", "INF Registered tunnel connection connIndex=0",
                     "INF Registered tunnel connection connIndex=1", "WRN Connection terminated connIndex=0"):
            count = remote_tunnel.follow(count, line)
        self.assertEqual(count, 1)

    def test_it_waits_for_the_server_and_stops_cleanly(self):
        spawned = []

        class FakeProcess:
            def __init__(self, argv, **kwargs):
                spawned.append(argv)
                self.returncode = None
                self.stdout = iter(["INF Registered tunnel connection connIndex=0\n"])
                self.killed = False

            def poll(self):
                return self.returncode

            def terminate(self):
                self.returncode = 1

            def wait(self, timeout=None):
                return self.returncode

            def kill(self):
                self.returncode = 1

        listening = threading.Event()
        tunnel = remote_tunnel.Tunnel(8090, command=lambda: ["cloudflared", "tunnel", "run", "defalt"],
                                      spawn=FakeProcess, enabled=lambda: True,
                                      listening=lambda port: listening.is_set())
        tunnel.start()
        time.sleep(0.3)
        self.assertEqual(spawned, [], "started before the server was listening")
        listening.set()
        deadline = time.time() + 3
        while tunnel.state != "online" and time.time() < deadline:
            time.sleep(0.02)
        self.assertEqual(tunnel.state, "online")
        process = tunnel._process
        tunnel.stop()
        self.assertEqual(process.returncode, 1)
        self.assertEqual(tunnel.status()["state"], "off")

    def test_the_console_runs_its_own(self):
        with patch.dict(os.environ, {"DEFALT_CONSOLE": "1", "REMOTE_TUNNEL": "defalt"}):
            self.assertIsNone(remote_tunnel.start_standalone(8090))


if __name__ == "__main__":
    unittest.main()
