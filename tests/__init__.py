"""Tests never read the real .env.

Credentials in a working checkout would otherwise switch on live Spotify,
OpenRouter or Steam calls inside tests that expect them off, as they are in
CI. This runs before radio.config imports load_dotenv.
"""
import dotenv

dotenv.load_dotenv = lambda *args, **kwargs: False
