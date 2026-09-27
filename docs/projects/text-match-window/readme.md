---
status: draft
priority: later
---

# Widen the text match past its 5,000-row window

**Goal:** Keep a long-quiet room findable by its name, and a contactless
address fragment reaching a long-quiet thread, past the newest 5,000 chat rows.

No plan of its own yet.
[resolver-windows.md §3](../resolver-windows/readme.md#3-what-remains-and-why-it-can-wait)
records what is left after #37: `fetch_chats` reads the newest
`NAME_SEARCH_SCAN` (5,000) rows, so the text match that still serves rooms and
address fragments cannot see past them. The failure is loud, affects rooms and
fragments only, and cannot happen below 5,000 chats. The database it was
measured on holds 1,165, which is why this waits.
