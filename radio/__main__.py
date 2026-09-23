"""python -m radio  -- put the station on the air."""
from __future__ import annotations

import signal
import sys

if __name__ == "__main__":  # before anything below can print: logs/station.log
    from .feedback import install_log_tee
    install_log_tee()

from . import config
from .app import create_app, shut_down_station

# The console starts us on this port too, and passes PORT explicitly anyway.
DEFAULT_PORT = 8090


def main() -> int:
    application = create_app()

    def goodbye(*_args: object) -> None:
        print("\nsigning off...", flush=True)
        # Once only: POST /api/shutdown may already have done it.
        shut_down_station()
        sys.exit(0)

    signal.signal(signal.SIGINT, goodbye)
    try:
        signal.signal(signal.SIGTERM, goodbye)
    except (AttributeError, ValueError):
        pass

    host = config.env("HOST", "127.0.0.1")
    port = int(config.env("PORT", str(DEFAULT_PORT)) or DEFAULT_PORT)
    identity = config.station.get("identity", {}) or {}
    print(f"{identity.get('name', 'station')} is on http://{host}:{port}",
          flush=True)
    # Remote listening, when this is not the console's station: the tunnel
    # waits for the server below to be listening, and goes down first.
    from . import remote_tunnel
    remote_tunnel.start_standalone(port)

    application.run(host=host, port=port, debug=False, threaded=True,
                    use_reloader=False)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
