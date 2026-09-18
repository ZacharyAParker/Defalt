"""python -m radio  -- put the station on the air."""
from __future__ import annotations

import signal
import sys

from . import config, director
from .app import create_app


def main() -> int:
    application = create_app()

    def goodbye(*_args: object) -> None:
        print("\nsigning off...", flush=True)
        director.station().shutdown()
        sys.exit(0)

    signal.signal(signal.SIGINT, goodbye)
    try:
        signal.signal(signal.SIGTERM, goodbye)
    except (AttributeError, ValueError):
        pass

    host = config.env("HOST", "127.0.0.1")
    port = int(config.env("PORT", "8080") or 8080)
    identity = config.station.get("identity", {}) or {}
    print(f"{identity.get('name', 'station')} is on http://{host}:{port}",
          flush=True)

    application.run(host=host, port=port, debug=False, threaded=True,
                    use_reloader=False)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
