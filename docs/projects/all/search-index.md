# Plan: Index message bodies ourselves

**Status:** Written down, deliberately not being built. The correctness fix it
would have sat on top of has landed — search reads the whole blob now rather than
the first 41 bytes of it — and the resulting speed was judged good enough, so
this is on hold rather than in progress. §7 records that decision and what would
reverse it. See [query-performance.md §10](query-performance.md) for the fix and
what it cost.

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
the socket and the log. Creating a file there is not new.

What would be in it is. The socket and the log carry no message history; an index
carries all of it, in plaintext, outside the TCC grant that guards `chat.db`.
That is a real change to the threat model — it is the fourth item in §5, and it
is why §7 ends with this not being built. The narrow point settled here is only
the one §4 of query-performance.md raised: nothing in this plan writes to
`chat.db`.

## 3 A partial index is still useful, which is what makes this tractable

The property that makes this worth building incrementally: **searches can be
bounded by date, so an index that only covers part of history is still usable for
all of it.**

Search the index for the range it covers, scan `chat.db` directly for the range
it does not, and merge. Correctness never depends on the index being complete —
only speed does. That means:

- No blocking first run. The daemon indexes new arrivals as they land and
  backfills behind them, so search works from the first minute.
- No migration story. An index that is missing, stale, corrupt, or half-built
  degrades to today's behaviour rather than to a wrong answer.
- The covered range grows from the newest end, which is the end people search. A
  search bounded to recent history is answered entirely from the index on the
  first day; only one reaching further back than the backfill has got needs a
  direct scan, and that range shrinks with every batch.

Coverage is therefore a single range ending at now, and the marker for it is one
row: the *oldest* `message.rowid` the backfill has reached. The newest end needs
no marker, because the daemon is already watching it. The only gap there is
between its last tick and now, and a date-bounded scan of that is trivially
cheap — 87ms buys a whole year
([query-performance.md §8](query-performance.md)).

## 4 Shape

- **FTS5** over decoded bodies, in `~/.local/state/msg/index.db`. One row per
  message: rowid, date, chat id, handle id, decoded body.
- **Built by the daemon**, which already holds Full Disk Access, is already
  resident, and already polls for new messages. Indexing new arrivals is the
  same tick that feeds watchers.
- **Backfilled in the background**, newest first and walking oldest-ward, in
  bounded batches, so it never competes with a query the user is waiting on and
  the useful end of history is covered first.
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
time](query-performance.md), which is worth doing on its own — and worth more
alongside this, not less. §3 leaves the oldest history unindexed longest, and
that is precisely the range a windowed search keeps from costing a full scan.

## 7 Why it is not being built

Search costs about 2.5 seconds unscoped and 236ms scoped to a person. An index
would make both effectively instant. It is not being built anyway, because the
gap between "a couple of seconds" and "instant" is not worth what §5 lists —
and one item there is not a cost at all but a change in kind.

**It would put a second copy of every message on disk in plaintext, outside the
protection the original has.** `chat.db` is guarded by TCC: reading it needs a
grant a human gave in System Settings, and this program's whole architecture
exists to keep that grant on one small daemon instead of on a terminal. An index
in `~/.local/state/msg/` has none of that. It is 0700 in a home directory, which
stops other users and stops nothing else — every process running as this user can
read it, with no grant and no prompt. That is the scope reduction in
[daemon-and-permissions.md §6](daemon-and-permissions.md) being handed back, and
for a search that is already fast enough to use.

The rest is ordinary cost, and it is all recurring rather than one-off: staleness
against deletions and edits, a rebuild path when the decoder changes, tens of
megabytes to account for and document a way to remove, and a second store that
every future filter has to be taught about or it silently answers from the wrong
place.

**What would reverse this.** Not a general wish for speed — a specific case where
seconds are actually wrong: search becoming interactive rather than one-shot,
where a keystroke is a query; or the database growing enough that a full scan
stops fitting the "couple of seconds" this decision rests on. If either happens,
§3 is the part to build first, because a partial index needs no migration and
degrades to today's behaviour rather than to a wrong answer.

Recorded rather than deleted, so the next person does not rediscover that
Spotlight is a dead end and BlueBubbles injects a dylib. §1 is the durable part
of this document; the design after it is only worth reading if §7 is revisited.
