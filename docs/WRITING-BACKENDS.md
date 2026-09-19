# Writing backends

Defalt can write host dialogue and process text requests through OpenRouter or a locally signed-in Codex CLI. Speech synthesis has its own provider settings. Changing the writer does not change voices, deck playback, or transition timing.

OpenRouter remains the default for new installations. To use Codex, install its CLI, sign in, and add this block to the private `config/overrides.yaml`:

```yaml
director_backend:
  provider: codex
  model: gpt-5.6-luna
  reasoning: medium
  utility_reasoning: low
  timeout_seconds: 25
  memory_vault: ""
  executable: ""
```

Set `memory_vault` to the optional curated vault path. Set `executable` only if the CLI cannot be found automatically. Both settings may contain local paths and should stay out of shared configuration. Keep the existing OpenRouter key configured for fallback. Set `provider: openrouter` to return to that writer.

Luna medium handles dialogue; low handles brief request, article, and metadata tasks. Utility tasks run without persistent conversations or personal memory. Dialogue uses a dedicated session that rotates after eight turns or an hour idle. Memory, persona, and model-setting changes reset it. Only one dialogue request uses that session at a time; concurrent requests use the fallback instead of waiting behind it.

The session timeout is capped to reserve part of each writing request's time for OpenRouter. Invalid JSON, unsupported dialogue, failed sessions, and unavailable CLI access also fall back. If neither provider returns usable text, segment writers use their existing fallback lines. The writing work runs on the radio's existing background workers, not its playback thread.

## Curated memory

Use a dedicated vault with a `Notes` folder. Each eligible Markdown note needs these properties and a `Facts` section:

```markdown
---
id: recording-preferences
status: active
broadcast: true
basis: direct
core: true
keywords: [music, recordings]
sources: [listener preference]
---
## Facts
- Prefer original studio recordings unless another version is requested.

## Sources
Local provenance or a link to the source note.
```

`basis` may be `direct` or `documented`. IDs must be unique lowercase slugs. Core notes are always included; up to two additional notes are selected by relevant keywords. Only the `Facts` section enters prompts. The loader does not follow source links or import another vault automatically. Keep facts brief and reviewed. Treat historic requests as history, not proof that the listener selected today's airing.

Disable a note with `broadcast: false`. An unreadable or invalid vault omits personal context rather than reusing stale facts. Session resets do not delete old provider history or already prepared audio. See the [privacy policy](../PRIVACY.md).

`/api/status` includes `llm.director` with the configured provider, model, reasoning, and latest result. It reports fallback without exposing credentials or memory contents. CLI helpers run headlessly with read-only permissions, shell and integrations disabled, and inherited application keys removed from their environment. The CLI uses its own existing authentication; Defalt does not copy it.

Codex calls use the signed-in account's allowance and cloud inference. Queue selection, repeat guards, beat timing, and audio effects remain controlled by the station. The writer receives current task data and produces text; it cannot directly manipulate decks or files.
