# Privacy policy

Effective September 19, 2026. Applies to Defalt 0.3.6 as distributed in this repository.

## The short version

Defalt runs on your machine, but several features use external services. Requests, listening history, host dialogue, and submitted articles can contain personal information. Only submit material you are comfortable sending to the providers you enable.

## What stays on your machine

Defalt stores track metadata, listening events, preferences, requests, vibe suggestions, article submissions, and station state in its local database. Audio, generated speech, analysis, and stems can be cached locally. Settings, credentials, diagnostic logs, and an optional Obsidian vault are separate local files. The vault can contain listening history and host transcripts.

The browser player uses local storage for playback preferences such as volume and mute. The distributed app does not include advertising trackers or an automatic analytics or crash-report upload service operated by the maintainer.

## What leaves your machine

- OpenRouter and the selected model provider receive prompts for enabled language-model features. These can include requests, track details, vibe instructions, selected listening-history facts, host dialogue, headlines, and the full text of submitted articles. API credentials authenticate these requests.
- If the Codex writing backend is enabled, the locally signed-in Codex CLI sends those writing prompts to OpenAI using its existing authentication. This is cloud inference. Selected facts from a configured director memory vault may be included in dialogue prompts. OpenRouter fallback receives the same selected facts. Original source documents and provenance paths are not automatically imported into prompts.
- Gemini speech through OpenRouter receives host lines and delivery instructions. Microsoft Edge TTS receives the written lines if the primary voice provider fails. Those lines can include personal details from your requests or history.
- YouTube receives searches, metadata requests, and requests for selected videos or audio. Spotify receives search queries and application credentials for its search feature; Spotify search does not provide playback audio.
- Cover-art lookup sends the track artist and title to Spotify when configured, and downloads matching album artwork from Spotify's image host. YouTube's image host receives the resolved video identifier when a thumbnail is needed. Images are cached locally; the studio scene itself is bundled with the app.
- Steam receives the configured SteamID and applicable credentials when library, wishlist, or news features are used. RSS publishers and article websites receive requests for their pages or feeds.
- The browser player loads fonts from Google Fonts. Optional model or dependency downloads contact their respective hosts.

Each destination can also receive network information, including your IP address, and ordinary request headers. Providers apply their own policies, retention practices, and terms. Defalt cannot promise that a provider will never retain data or use it for training. Local audio analysis does not itself upload your music files to a language model.

Director chat messages and relevant station context are sent to the configured writing provider. "Private" means excluded from host dialogue, not hidden from that provider. Chat history is held in backend memory; saved music directions remain in local settings. Explicitly shared messages enter the normal on-air request and transcript flow. Ordinary private chat does not update long-term taste scores.

Explicit requests to commission an ad send the requested premise and tone to the host writer. Unrelated private conversation is excluded from that brief. News-based ad requests fetch material from the selected configured news category. The resulting speech enters the normal ad and transcript flow.

## Retention and control

Audio cache settings control cached audio retention; they do not erase listening history, article text, requests, logs, transcripts, or files saved elsewhere. Clearing or dismissing a request changes its queue state and is not a promise of permanent deletion. Unavailable sources can be retried after 24 hours; that waiting period does not automatically delete the stored identifier.

You control local files and backups. To remove local records, close Defalt and its backend before removing the relevant database, cache, logs, or vault files. Back up anything you want to keep; removing the database also removes library metadata and preferences. Browser site-data controls remove browser-local preferences. Provider-side deletion must be handled with that provider.

Director memory notes can be excluded by setting `broadcast: false`, or the memory-vault setting can be cleared. Changed memory resets the dialogue session on its next request. Already prepared speech may still contain earlier context. Changing or deleting notes does not erase previous Codex session history, generated transcripts, cached speech, or provider records. Codex manages its own local session history and account usage separately from Defalt.

Disable integrations you do not want to use and remove their credentials. Avoid putting private information in article requests, vibe prompts, host instructions, or files you plan to share. Credentials are stored in local configuration, not a dedicated encrypted credential vault.

## Local server and sharing

The server binds to loopback by default and has no public-facing authentication. Keep it on your own machine. Other people or software with access to your files or local server may be able to read station data. Do not publish your credentials, database, audio cache, logs, or vault.

## Updates and contact

Material changes to these practices will be reflected in this document with a new effective date. The version bundled with a native executable describes that build; the repository may describe a newer release. Contact the maintainer through the Defalt repository on GitHub. Do not include credentials, private listening records, or other sensitive data in public issues.

---
Defalt v0.3.6 · © 2026 Zachary Parker
