---
status: draft
priority: later
---

# Show one thread on demand

**Goal:** Show every message in a thread, given any message in it, without
regrouping the transcript.

No plan of its own yet.
[threading.md §8](../threading/readme.md#8-slice-2-is-not-being-built-and-what-might-be-instead)
records the shape: resolve the given message to its thread first, then take
everything sharing it. Regrouping the whole transcript by thread is
deliberately not being built, and that section says why.

It needs message ids to be visible first, because rendered output prints no
rowid for a reader to type. That is the same problem attachments solved by
printing their ids, and §8 names the `↳ replying to …` line as the place to put
the originator's rowid. Build that piece first if this is picked up.
