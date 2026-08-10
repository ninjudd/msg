---
status: Shipped
---

# Plan: Make the common commands stop taking two seconds

**Status:** Done, in the sense that matters: the chat list went from ~2.1s to
~150ms, and `search` was found to have never worked at all and now does. Search
costs a couple of seconds unscoped and 236ms scoped to a person, which was
judged fast enough to stop here — §9 and [search-index.md](search-index.md)
record the two ways to go faster, and neither is being built.

§1 to §4 are the diagnosis as it was written, before any of it was acted on. §6
records what was done and what it cost. §8 corrects a claim §7 made about early
exit that turned out to be false when measured, and §9 is the plan that replaces
it. **§10 matters most: every measurement before it timed a predicate that was
not actually searching message bodies.** Read it before trusting any number
above. §11 is the limit bug that fixing the predicate exposed, and §12 is the
case folding it got wrong for anything outside ASCII.

Nothing here was a regression from [the Rust rewrite](rust-rewrite.md): the same
numbers came out of the TypeScript build, and that work simply removed
everything else that was slow, which left this as the only thing to look at.

**Goal:** Get `msg chats` and `msg read` under a couple of hundred milliseconds
on a database with a decade of messages in it.

## 1 What it costs today

Measured against a real database — 763,304 messages, 2,802 conversations, 1,164
of them unfiltered — through a warm daemon, three runs averaged.

| Command | Time |
| --- | --- |
| `msg --version` | 32ms |
| `msg daemon status` | 36ms |
| `msg chats -n 1` | 2053ms |
| `msg chats -n 3000` | 2263ms |
| `msg send 1 x --dry-run` | 1721ms |
| `msg read 1 -n 5` | 2341ms |
| `msg search -n 5 <no match>` | 2129ms |
| `msg search -n 5 -c 1 <no match>` | 2824ms |

Startup is 32ms and the daemon answers `SELECT MAX(rowid)` in 36ms, so
essentially all of the rest is one of two queries.

## 2 Two independent costs, and nearly every command pays one

**`fetch_chats` costs about 1.7 to 2.0 seconds, whatever you asked for.** The
tell is in the table: `-n 1` and `-n 3000` differ by 10%. `CHATS_SQL` runs four
correlated subqueries per chat row — a `GROUP_CONCAT` over `chat_handle_join`
joined to `handle`, a `COUNT` over `chat_handle_join`, a `MAX(message.date)`
over `message` joined through `chat_message_join`, and a `COUNT` over
`chat_message_join` — and only *then* does the outer query order by `lastDate`
and apply the `LIMIT`. Every conversation is fully costed before all but a
handful are thrown away.

**It is not only `msg chats`.** `resolve_chat` is built on `fetch_chats`, so
anything that names a conversation pays it: `read`, `send`, `send --dry-run`,
`watch -c`, and `search -c`. `send --dry-run` at 1721ms is that cost alone, with
no messages fetched at all.

**`search` costs about 2.1 seconds separately.** Its `WHERE` includes
`CAST(message.attributedBody AS TEXT) LIKE ?`, which cannot use an index and so
reads and casts every blob in the table. The two costs are additive rather than
shared: `search -c 1` is 2824ms, which is roughly one of each.

## 3 Directions, none of them committed

- **Replace the correlated subqueries with grouped joins.** Three of the four
  aggregate over the same two tables and could be one `GROUP BY` pass each,
  joined to `chat` once.
- **Order and limit before aggregating.** The list is sorted by last activity,
  so a cheap `MAX(date)` per chat could pick the top N and only those N would
  need member counts and names. This is probably the largest single win and
  changes nothing a caller can observe.
- **Give `resolve_chat` its own query.** Naming one conversation does not need
  the whole list built, sorted, and truncated first; a rowid especially does not.
- **Check what indexes the real schema actually has** before assuming any of the
  above helps. [AGENTS.md](../../../AGENTS.md) is emphatic that this repository
  has been wrong about the schema before, and an `EXPLAIN QUERY PLAN` against a
  real database is the first thing to run, not the last.
- **For `search`, narrow before scanning.** A date bound or a chat bound applied
  first would cut the blob scan; `--since` already exists and is not being used
  to reduce work.

## 4 What not to do

**Do not cache the chat list in the daemon.** It is resident and messages arrive
constantly, so a cache is either stale — which is worse than slow, because the
list is sorted by recency and a stale one is wrong at the top — or invalidated on
every insert, which is most of the time on an active machine.

**Do not reach for FTS.** It would mean writing to a database this program is
careful to only ever open read-only, which is a much larger change to the threat
model than a slow search is worth.

## 5 Why this was not fixed during the rewrite

The port's rule was that behaviour should be identical to the TypeScript so the
two could be compared, byte for byte, at every layer. Rewriting a query would
have made the outputs differ for a good reason, and then a real difference could
have hidden behind it. Fixing it now, against a build with tests and a fixture,
is both easier and safer.

## 6 What was done

**The chat list reads a date that Messages already denormalised.**
`chat_message_join` carries a `message_date` column — a copy of `message.date`
kept in step by trigger — and there is an index on
`chat_message_join(chat_id, message_date, message_id)`. So the whole
last-activity-and-count aggregate is answered by one covering-index scan that
never opens `message`. The old query's `MAX(message.date)` needed one random
probe into `message` per join row, and there are as many join rows as messages.

The query plan is the clearest statement of it. Before:

```
CORRELATED SCALAR SUBQUERY 3
  SEARCH chat_message_join USING COVERING INDEX ... (chat_id=?)
  SEARCH message USING INTEGER PRIMARY KEY (rowid=?)      <- 733,690 times
```

After:

```
MATERIALIZE recent
  SCAN chat_message_join USING COVERING INDEX chat_message_join_idx_message_date_id_chat_id
```

**§4 of [AGENTS.md](../../../AGENTS.md) earned its place again.** The plan above
guessed the fix would be restructuring the correlated subqueries into grouped
joins. That guess was half right and would have bought much less: the win came
from a column nobody had looked for. Reading the schema first, rather than
reasoning from the query text, is what found it. The schema was read out of the
daemon's own log, because the terminal holds no Full Disk Access — DDL and query
plans only, no rows.

**The copy was verified before being trusted**, over the 733,690 join rows of a
real database: none were zero, none were NULL, none disagreed with
`message.date`, no conversation's maximum differed, and there were no orphan join
rows. A test keeps the fixtures honest, so a fixture row added without a date
fails loudly rather than making the chat list quietly wrong.

Measured on the same database, and the outputs are hash-identical to before
across six query shapes:

| Command | Before | After |
| --- | --- | --- |
| `msg chats -n 1` | 2053ms | 218ms |
| `msg chats -n 500` | 2284ms | 139ms |
| `msg chats -n 3000` | 2263ms | 136ms |
| `msg chats dana` | 2226ms | 152ms |
| `msg send 1 x --dry-run` | 1721ms | 148ms |
| `msg read 1 -n 5` | 2341ms | 290ms |
| `msg search -n 20 -c 1 <no match>` | 2824ms | 281ms |
| `msg search -n 20 <no match>` | 2129ms | 1491ms |

The cost is also flat in the limit now, which was §2's tell that the work was
being done regardless: `-n 1` and `-n 3000` are within 80ms of each other, and
the larger one is *faster* because it skips the temporary B-tree less often.

## 7 What is left

**`search` is now the slowest thing here**, at about 1.5 seconds for a query
that matches nothing. It is unchanged by the above and remains a full scan:
`CAST(message.attributedBody AS TEXT) LIKE ?` cannot use an index, and a query
with no matches has to read every blob before it can say so. A query that
matches plenty is much faster, because `ORDER BY date DESC LIMIT n` lets it stop
early.

Two things that look like fixes and are not. Replacing `LIKE` with `instr` on
the raw blob skips building a string per row, but `instr` is case-sensitive
where `LIKE` is not, so it would silently stop finding case-variant matches.
Testing `message.text` before casting the blob saves nothing, because 97.6% of
messages have no `text` at all.

**`resolve_chat` still builds the whole list to find one conversation.** It is
now cheap enough not to matter — `read` went from 2341ms to 290ms, roughly half
of which is still this — but pushing the chat id into the aggregate would make
it a single-row lookup. It needs a second SQL string, which is a second thing to
keep in step, so it is worth doing only if `read` starts to feel slow again.

## 8 §7 was wrong about the early exit

Measured on the same database, warm daemon, three runs:

| Query | Time |
| --- | --- |
| a word in a large share of all messages, `-n 20` | 1382ms |
| the same word, `-n 200` | 1401ms |
| no match at all, `-n 20` | 1440ms |
| no match, `--since 30d` | 92ms |
| no match, `--since 1y` | 87ms |

§7 said a query that matches plenty stops early and is therefore much faster.
**It does not, and it is not.** Matching heavily costs the same as matching
nothing, and raising the limit tenfold changes nothing either — the tell that no
early exit is happening. What `ORDER BY date DESC LIMIT n` gets is a sort of the
whole result, not a walk that stops.

The row that matters is the last one. A bound on `message.date` *is* used, and it
cuts the work by twenty times, which says the cost is proportional to the span
searched rather than to the matches found. That is the lever.

## 9 Searching backwards through time, and streaming

The plan, not yet built. Walk backwards in widening windows — a week, a month, a
quarter, a year, then everything older — each one a bounded query of the kind §8
measured at under 100ms, and emit matches as each window lands.

Two properties make it worth doing rather than merely faster-feeling:

- **It can stop.** Windows run newest first, so once `limit` matches are in hand
  the older windows cannot contain a newer match by definition. This is the early
  exit §7 wrongly assumed was already there — as a property of how the search is
  driven, rather than something the query planner has to be talked into.
- **It streams.** The newest matches are usually the wanted ones, and they arrive
  from the first window rather than after the whole history is read. The daemon
  already has the frame for it: `watch` sends `item` frames and the client reads
  them as they come.

The windows must be half-open and non-overlapping, or a message on a boundary is
returned twice or skipped. A no-match query still reads everything and so stays
around the current cost, plus a few milliseconds per window; that is the trade,
and it is the right way round, because the case that gets slower is the one where
there is nothing to show anyway.

One thing to keep straight: the SQL `LIMIT` applies to raw blob matches, and the
decoded-body filter narrows them afterwards, so a window can return fewer results
than it looked like it would. Counting toward the limit has to happen after that
filter, not before.

## 10 The measurements above were of a broken predicate

Everything before this section timed a search that was not searching. The clause
was `CAST(message.attributedBody AS TEXT) LIKE ?`, and SQLite hands a cast blob to
`LIKE` as a NUL-terminated string. A typedstream blob has NULs in its header,
well before the text: measured, an 88-byte blob casts to 41. So the match only
ever saw the archive header, and the 97.6% of messages whose body lives in
`attributedBody` could not be found at all. Only the 2.4% that also fill
`message.text` ever matched.

It was not a regression. The identical clause is in the original TypeScript at
`src/db.ts:246`, so search had never worked properly; the Rust port carried it
over faithfully, bug included.

This retires §3's last bullet and §7's second paragraph. §7 ruled out `instr` for
being case-sensitive where `LIKE` is not — but `instr` is the one that would have
worked, precisely because it does not stop at the NUL. The real constraint was
never case sensitivity, it was that one predicate read the whole blob and the
other did not.

The fix is a scalar function registered on the connection, `msg_body_has(text,
attributedBody, needle)`, doing a case-insensitive byte scan in Rust — a byte
scan only for ASCII needles, as §12 goes on to correct. Sound
rather than approximate: the decoder reads a slice of those same bytes as UTF-8,
so anything surviving into the decoded body is present in the blob, which makes
this a superset of what the decoded filter accepts — what a prefilter has to be.

**Correct is slower than wrong**, and the numbers move accordingly, because the
predicate now reads whole blobs rather than 41 bytes of each:

| Query | Before (wrong) | After (correct) |
| --- | --- | --- |
| `DFI` | 0 results, 1440ms | 20 results, 2434ms |
| `dinner` | 0 results | 20 results, 2334ms |
| no match at all | 1440ms | 2569ms |
| `--from <person> DFI` | 0 results | 20 results, 236ms |

That last row is the shape of the answer. A person filter rejects rows on an
integer before the blob is ever read, and it is ten times faster than the
unscoped search as a result. Narrowing before scanning is what works; §9 does it
by date, and [search-index.md](search-index.md) removes the scan entirely.

## 11 The limit was under-delivering, not truncating

Fixing the predicate exposed the wart §9 had predicted. `LIMIT` bounds *raw*
matches and the decode-and-check narrows them afterwards, so a needle found in
archived metadata rather than in the visible body consumed one of the results
asked for instead of being replaced by the next real one. Asking for 100 returned
99 while 247 matched.

The fix is to over-fetch and trim. The first ask is `limit * 4 + 64`, widening
fourfold if even that comes up short, and stopping when the database returns
fewer rows than asked — which is what proves there is nothing further back.

Over-fetching on the *first* pass rather than retrying matters, and §8 is why: a
wider `LIMIT` costs almost nothing because there is no early exit, while a second
pass re-runs the whole scan. Measured, recovering by retry took `-n 100` from
1948ms to 5490ms; recovering by asking wide once takes it to 2412ms, and the cost
is flat in the limit again.

| `-n` | Under-delivering | Retrying | Asking wide once |
| --- | --- | --- | --- |
| 100 | 99 results, 1948ms | 100, 5490ms | 100, 2412ms |
| 247 | — | 247, 4522ms | 247, 2596ms |
| 1000 | — | 247, 2686ms | 247, 2386ms |

A cursor would beat all three, continuing from the oldest row already seen rather
than re-reading from the top. That is §9's windowing, and it is the reason to
build it.

## 12 Case is a property of characters, not of bytes

§10's byte scan folded case a byte at a time, which is only case folding inside
ASCII: `É` and `é` differ in both of their bytes, so `café` did not find `CAFÉ`.
Worse than a missing convenience, because this is the prefilter — a row it
rejects never reaches the decoded filter, which folds properly and would have
accepted it. The two disagreed, and the stricter one ran first.

They cannot disagree now, because they are one function: the decode-and-check
calls the same predicate the SQL prefilter does, differing only in what it is
handed. A needle outside ASCII decodes the blob and folds per character. Since
the framing between the text is not valid UTF-8 and text never spans it, each
valid run is searched on its own and the framing never matches.

An ASCII needle keeps the byte scan, which is why the common case did not get
slower. Lowercasing an ASCII character never leaves ASCII, so only an ASCII
character can match one — near enough: `K` U+212A lowercases to `k`, and that
curiosity is knowingly not found.

| Needle | Cost |
| --- | --- |
| ASCII, matching | 2.15s |
| ASCII, matching nothing | 2.05s |
| Outside ASCII | 4.15s |

The ASCII rows are the 2.4s §11 measured, within noise, so the common search did
not get slower. A needle outside ASCII costs roughly twice that, and only
searches that need decoding pay it. Left there deliberately: the exact
alternative is a reverse fold table, and an approximate one is a second predicate
that disagrees with the first, which is the bug this section is about. If that
4.15s ever matters, [search-index.md §7](search-index.md) is the decision to
revisit rather than this scan to sharpen.
