"""Remote access: which hosts may reach the station, and proof they may.

The station binds loopback only. A Cloudflare Tunnel brings a public name
(REMOTE_HOSTS) to that loopback port, and Cloudflare Access sits in front of
it. Every request that arrives under a remote name -- or that came through
Cloudflare at all -- must carry the Access token Cloudflare signed for it:
the `Cf-Access-Jwt-Assertion` header, or the `CF_Authorization` cookie.

The token is an RS256 JWT, checked here against the team's published keys
without any crypto library: RSASSA-PKCS1-v1_5 verification is one modular
exponentiation and a byte comparison. Everything fails closed -- no keys, no
configuration, a clock problem, a malformed token: refused.
"""
from __future__ import annotations

import base64
import hashlib
import hmac
import json
import threading
import time
import urllib.request
from typing import Any, Callable

from . import config

KEYS_TTL = 3600.0        # how long fetched keys are trusted
UNKNOWN_KID_EVERY = 30.0  # an unknown key id refetches at most this often
LEEWAY = 60.0             # clock skew allowed on exp / nbf
FETCH_TIMEOUT = 5.0

# DER prefix of DigestInfo for SHA-256 (RFC 8017, section 9.2 note 1).
SHA256_PREFIX = bytes.fromhex("3031300d060960864801650304020105000420")


class Refused(Exception):
    """Why a request was not let in. Logged, never shown in detail."""


def remote_hosts() -> set[str]:
    return {h.lower().strip().rstrip(".") for h in config.env_list("REMOTE_HOSTS")}


def team_domain() -> str:
    raw = config.env("CF_ACCESS_TEAM_DOMAIN").strip().lower()
    for prefix in ("https://", "http://"):
        if raw.startswith(prefix):
            raw = raw[len(prefix):]
    return raw.strip("/")


def audience() -> str:
    return config.env("CF_ACCESS_AUD").strip()


def configured() -> bool:
    """Access is set up well enough to let anybody in remotely."""
    return bool(remote_hosts() and team_domain() and audience())


def is_remote_host(name: str) -> bool:
    return name.lower().rstrip(".") in remote_hosts()


# --------------------------------------------------------------------------
# base64url, JWK
# --------------------------------------------------------------------------
def b64url(text: str) -> bytes:
    if not isinstance(text, str) or any(c in text for c in "+/= \n"):
        raise Refused("not base64url")
    try:
        return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))
    except (ValueError, TypeError) as error:
        raise Refused("not base64url") from error


def _int(text: str) -> int:
    return int.from_bytes(b64url(text), "big")


def parse_jwks(document: Any) -> dict[str, tuple[int, int]]:
    """{kid: (n, e)} for every usable RSA signing key in a JWKS document."""
    keys: dict[str, tuple[int, int]] = {}
    for key in (document or {}).get("keys") or []:
        if not isinstance(key, dict) or key.get("kty") != "RSA":
            continue
        if key.get("use", "sig") != "sig" or key.get("alg", "RS256") != "RS256":
            continue
        try:
            n, e = _int(key["n"]), _int(key["e"])
        except (KeyError, Refused):
            continue
        # Nothing weaker than 2048 bits is a real Access key.
        if n.bit_length() >= 1024 and e > 1 and isinstance(key.get("kid"), str):
            keys[key["kid"]] = (n, e)
    return keys


# --------------------------------------------------------------------------
# RS256
# --------------------------------------------------------------------------
def rs256_valid(message: bytes, signature: bytes, n: int, e: int) -> bool:
    """RSASSA-PKCS1-v1_5 with SHA-256, verification only (RFC 8017 8.2.2)."""
    size = (n.bit_length() + 7) // 8
    if len(signature) != size:
        return False
    s = int.from_bytes(signature, "big")
    if s >= n:
        return False
    encoded = pow(s, e, n).to_bytes(size, "big")
    digest = SHA256_PREFIX + hashlib.sha256(message).digest()
    padding = size - len(digest) - 3
    if padding < 8:
        return False
    expected = b"\x00\x01" + b"\xff" * padding + b"\x00" + digest
    return hmac.compare_digest(encoded, expected)


# --------------------------------------------------------------------------
# Keys
# --------------------------------------------------------------------------
def _fetch_certs(url: str) -> Any:
    request = urllib.request.Request(url, headers={"User-Agent": "Defalt"})
    with urllib.request.urlopen(request, timeout=FETCH_TIMEOUT) as response:  # noqa: S310 - https, fixed host
        return json.loads(response.read(256 * 1024).decode("utf-8"))


class KeyCache:
    """The team's signing keys, refetched hourly or when a new kid turns up."""

    def __init__(self, fetch: Callable[[str], Any] = _fetch_certs,
                 clock: Callable[[], float] = time.monotonic) -> None:
        self._fetch = fetch
        self._clock = clock
        self._lock = threading.Lock()
        self._team = ""
        self._keys: dict[str, tuple[int, int]] = {}
        self._fetched = -1e18
        self._tried_unknown = -1e18

    def _refresh(self, team: str) -> None:
        document = self._fetch(f"https://{team}/cdn-cgi/access/certs")
        keys = parse_jwks(document)
        if keys:
            self._keys, self._team, self._fetched = keys, team, self._clock()

    def key(self, team: str, kid: str) -> tuple[int, int] | None:
        with self._lock:
            now = self._clock()
            stale = team != self._team or now - self._fetched > KEYS_TTL
            unknown = kid not in self._keys and now - self._tried_unknown > UNKNOWN_KID_EVERY
            if stale or unknown:
                if unknown and not stale:
                    self._tried_unknown = now
                try:
                    self._refresh(team)
                except Exception as error:  # noqa: BLE001 - keep the last good keys
                    if config.DEBUG:
                        print("[access] could not fetch keys:", repr(error), flush=True)
            if team != self._team:
                return None
            return self._keys.get(kid)


keys = KeyCache()


# --------------------------------------------------------------------------
# Tokens
# --------------------------------------------------------------------------
def verify(token: str, *, team: str, aud: str, cache: KeyCache | None = None,
           now: float | None = None) -> dict[str, Any]:
    """The token's claims if Cloudflare Access issued it for us, else Refused."""
    if not team or not aud:
        raise Refused("Access is not configured")
    if not isinstance(token, str) or token.count(".") != 2 or len(token) > 16384:
        raise Refused("not a JWT")
    head_b64, body_b64, sig_b64 = token.split(".")
    try:
        header = json.loads(b64url(head_b64))
        claims = json.loads(b64url(body_b64))
    except ValueError as error:
        raise Refused("unreadable JWT") from error
    if not isinstance(header, dict) or not isinstance(claims, dict):
        raise Refused("unreadable JWT")
    if header.get("alg") != "RS256":
        raise Refused("unexpected algorithm")
    kid = header.get("kid")
    if not isinstance(kid, str):
        raise Refused("no key id")
    key = (cache or keys).key(team, kid)
    if key is None:
        raise Refused("unknown signing key")
    if not rs256_valid(f"{head_b64}.{body_b64}".encode("ascii"), b64url(sig_b64), *key):
        raise Refused("bad signature")

    audiences = claims.get("aud")
    audiences = [audiences] if isinstance(audiences, str) else audiences
    if not isinstance(audiences, list) or aud not in audiences:
        raise Refused("wrong audience")
    if claims.get("iss") != f"https://{team}":
        raise Refused("wrong issuer")
    moment = time.time() if now is None else now
    exp, nbf = claims.get("exp"), claims.get("nbf")
    if not isinstance(exp, (int, float)) or moment > exp + LEEWAY:
        raise Refused("expired")
    if nbf is not None and (not isinstance(nbf, (int, float)) or moment < nbf - LEEWAY):
        raise Refused("not valid yet")
    # A person (email) or a service token (common_name, for the app later).
    if not (claims.get("email") or claims.get("common_name")):
        raise Refused("no identity")
    return claims


def identity(claims: dict[str, Any]) -> str:
    return str(claims.get("email") or f"service:{claims.get('common_name')}")


def token_from(headers: Any, cookies: Any) -> str:
    return (headers.get("Cf-Access-Jwt-Assertion") or cookies.get("CF_Authorization") or "").strip()


def check(headers: Any, cookies: Any) -> dict[str, Any]:
    """Verify a request's Access token under the current configuration."""
    if not configured():
        raise Refused("REMOTE_HOSTS is set but Cloudflare Access is not configured")
    return verify(token_from(headers, cookies), team=team_domain(), aud=audience())
