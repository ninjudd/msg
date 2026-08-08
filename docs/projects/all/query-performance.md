# Plan: Make the common commands stop taking two seconds

**Status:** Measured, not started. Nothing here is new: the same numbers came
out of the TypeScript build, so this is not a regression from
[the Rust rewrite](rust-rewrite.md) — that work simply removed everything else
that was slow, which left this as the only thing left to look at.

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
