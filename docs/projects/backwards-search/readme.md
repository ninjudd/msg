---
status: draft
priority: later
---

# Search backwards through time and stream the results

**Goal:** Walk `msg search` backwards in widening windows, newest first, so it
can stop once `limit` matches are in hand and stream each window's matches as
they land.

No plan of its own yet. The design is
[query-performance.md §9](../query-performance/readme.md#9-searching-backwards-through-time-and-streaming),
written before this became a separate project; build from there. This entry
carries two lines `later.md` held before the Projector migration:

- **The speed.** Search is around 2.5s unscoped and 236ms scoped to a person,
  which was judged fast enough, so this is not urgent.
- **The slow tail.** A short needle that rarely begins a word walks most of the
  table and can take 15–30 seconds.
  [search-boundaries.md §6](../search-boundaries/readme.md#6-it-makes-the-widening-loop-work-harder)
  measured it and shipped anyway, since those searches previously returned
  noise instantly. Backwards streaming is the shape of a fix, because it bounds
  how much of the table a search has to read before it answers.

[search-index](../search-index/readme.md) records the other way to go faster,
which is deliberately not being built.
