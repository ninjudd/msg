# Plan: Index message bodies ourselves

**Status:** Not started. The correctness fix it replaces has landed — search now
reads the whole blob rather than the first 41 bytes of it — so this is about
speed, not about finding messages at all. See
[query-performance.md §10](query-performance.md) for what that fix cost.

**Goal:** Make `msg search` answer in the tens of milliseconds, without writing
to `chat.db` and without asking for any permission the daemon does not already
hold.

## 1 Why there is nothing to reuse

Three places a ready-made index might live, and none of them is available.

**Spotlight does not index messages, for us.** Measured on a real machine:
`mdfind` for a term known to occur in hundreds of messages returns only files —
source, notes, documents — and zero message items. There is no Messages importer
in `/System/Library/Spotlight`, and the user's `CoreSpotlight` store is empty.
Messages.app's own search bar is in-app, not a Spotlight query. `CSSearchQuery`
would only ever return items this program had itself indexed, which is this
document rather than a shortcut past it.

**`chat.db` carries no full-text index.** There is no FTS table and no index that
helps a body match, which is unsurprising: `attributedBody` is an archived object
graph, and SQLite cannot index inside one.

**BlueBubbles does not search the database.** Worth stating because it is the
most mature open-source reader of `chat.db` and it reached a different answer:
`searchMessagesPrivateApi` sends a `search-messages` action to a dylib injected
into Messages.app, which asks Apple's IMCore for the real index and returns
message GUIDs; `chat.db` is then read only to hydrate them. That is a good design
for a bridge and the wrong one here. Injecting into Messages.app needs SIP and
library validation weakened, and this program's entire architecture exists to
make the permission surface smaller — see
[daemon-and-permissions.md §1](daemon-and-permissions.md). Trading SIP for a
faster search would be the largest possible step backwards.

So the index has to be ours.

## 2 Where it goes, and why that is not §4 of query-performance

[query-performance.md §4](query-performance.md) says not to reach for FTS,
because it would mean writing to a database this program opens read-only. That
objection is about `chat.db`, and it still stands — nothing here writes there.

A **separate** database in `~/.local/state/msg/` is a different proposition. It
is the daemon's own state directory, already owner-only at 0700, already holding
the socket and the log. Writing there is not a change to the threat model: the
daemon already creates files in it.

## 3 A partial index is still useful, which is what makes this tractable

The property that makes this worth building incrementally: **searches can be
bounded by date, so an index that only covers part of history is still usable for
all of it.**

Search the index for the range it covers, scan `chat.db` directly for the range
it does not, and merge. Correctness never depends on the index being complete —
only speed does. That means:

- No blocking first run. The daemon can index backwards through history in the
  background while search keeps working from the first minute.
- No migration story. An index that is missing, stale, corrupt, or half-built
  degrades to today's behaviour rather than to a wrong answer.
- The unindexed tail is the *newest* messages, and a date-bounded scan of recent
  history is cheap — measured at 87ms for a year
  ([query-performance.md §8](query-performance.md)).

The high-water mark is one row: the newest `message.rowid` the index has seen.

## 4 Shape

- **FTS5** over decoded bodies, in `~/.local/state/msg/index.db`. One row per
  message: rowid, date, chat id, handle id, decoded body.
- **Built by the daemon**, which already holds Full Disk Access, is already
  resident, and already polls for new messages. Indexing new arrivals is the
  same tick that feeds watchers.
- **Backfilled in the background**, oldest-ward, in bounded batches, so it never
  competes with a query the user is waiting on.
- **Rebuilt from nothing when it does not match**, keyed on the Messages
  database's identity and the decoder's version. A decoder change means the
  stored text is wrong, and that has to force a rebuild rather than be noticed
  later.

## 5 What has to be got right

- **Deletions and edits.** Messages can be deleted and, since Ventura, edited.
  The index will drift from `chat.db` and needs a reconciliation pass —
  cheap to detect by comparing counts and maximum rowid per chat, expensive to
  do exactly. Decide how much drift is tolerable before building.
- **Size.** Roughly 763k bodies; the text itself is tens of megabytes, and FTS5
  adds its own index on top. Worth measuring before committing, and worth a
  documented way to delete it.
- **Tapbacks and attachments** are messages too, and mostly noise in a body
  search. The existing filters have to keep working against the index, which
  means the index stores enough columns to answer them without rejoining.
- **It is a second copy of the user's messages, in plaintext, outside the
  protection `chat.db` gets from TCC.** This is the part that needs the most
  care and the clearest documentation: 0700 is not the same protection Full Disk
  Access gives, and someone who never knew the file existed cannot make a
  judgement about it. It has to be announced, easy to remove, and probably
  refusable.

## 6 What this is not

Not a reason to delay [searching backwards through
time](query-performance.md), which is worth doing on its own: it bounds the work
on the unindexed range, and this plan depends on that range being cheap to scan.
