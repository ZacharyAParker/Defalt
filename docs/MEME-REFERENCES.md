# Song and artist memes

Personal song commentary can use the reviewed references in
[`config/memes.yaml`](../config/memes.yaml). Each entry includes its source,
review date, scope, and a short authored joke. Song memes require matching
artist credits and the exact title (ignoring case and punctuation). Artist
callbacks identify their original clip or song. Covers and unknown tracks
do not inherit a song meme from a shared title.

The initial catalog contains 11 references, reviewed September 18, 2026.
This is a curated catalog, not live internet search. If there is no match,
the hosts use ordinary song and listening-history jokes. Add references only
after checking their sources; the model's recollection is not verification.

Under **Hosts and speech**, enable personal commentary and verified memes.
Defaults are a 30% chance per personal break, at least 10 minutes between
memes, and 48 hours before reusing one. The quote toggle uses an original
unquoted joke instead. Allowed exact excerpts are capped at 10 words.

Cooldowns are stored in the station database when a break is **prepared**,
including breaks later discarded or skipped. This conservative rule prevents
queue rebuilds and restarts from repeatedly preparing the same joke. Recent
identical host lines are also excluded. Both outgoing and incoming songs can
match; only one reference is selected per break.

The opening is supplied verbatim; the dialogue model supplies a short reply
using the existing roast intensity and listening evidence. For introductions,
the other host finishes with the incoming title and artist. The authored
exchange also works when the dialogue model is unavailable. No web lookup
is added to playback or transition preparation.

The source, review date and matching song travel with the voice item and
transcript API as `reference` metadata. Existing transcript displays continue
to show spoken words; they do not yet show clickable source links. Sources
and exact catalog coverage are inspectable in the YAML above.
