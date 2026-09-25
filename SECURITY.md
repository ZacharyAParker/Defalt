# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue. On the Defalt repository on GitHub, open the **Security** tab and choose **Report a vulnerability** (GitHub private vulnerability reporting). That creates a private security advisory only the maintainer can see.

Include what you found, the version (it's in the app footer), how to reproduce it, and what someone could do with it. Don't include real credentials, `.env` contents, listening history or other private data; redact them or use made-up values.

You'll get a reply as soon as the maintainer can look at it. This is a personal project maintained by one person, so there is no bug bounty and no guaranteed response time. Please give a reasonable amount of time for a fix before talking about the problem publicly.

## Supported versions

Only the latest release gets security fixes. Update to it before reporting, and check whether the problem is still there.

## Scope

Defalt is a local Windows application. The console, the radio backend (127.0.0.1:8090 by default) and the stream server (127.0.0.1:8091) bind to loopback and have no login of their own; they rely on staying on loopback.

The one supported remote path is remote listening, described in docs/REMOTE.md. It depends on your own Cloudflare Tunnel and a Cloudflare Access application in front of it, plus the station's own check of the Cloudflare Access token (a signed JWT) on every request that arrives under the public name. Useful reports include ways to reach the station or the stream from outside without a valid Access token, cross-site requests the station accepts, paths that read files outside what the app serves, or secrets that end up in logs, bug reports or the browser.

Out of scope: problems that need someone who already controls your Windows account or files; setups that expose the local server directly to a network, which the documentation says not to do; misconfigured Cloudflare Access policies; and vulnerabilities in third-party services or tools such as Cloudflare, YouTube, Spotify, FFmpeg or cloudflared, which should go to those projects.

---
Defalt v0.4.4 · © 2026 Zachary Parker
