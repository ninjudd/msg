---
status: Shipped
---

# Plan: Show what was said around a search hit

**Status:** Shipped, one slice, as designed. `-A`/`-B`/`-C` on `msg search`,
windows merged into runs, the `> ` gutter and `--`, tapbacks in the window —
since reversed by tapbacks.md slice 1, see the §3 correction — and
`matched`/`group` on the wire. §2's cost estimate was wrong until the query was
reshaped; see the note there.

**Goal:** Let `msg search` print the messages on either side of each hit, the
way `ack -A/-B/-C` does, so a match reads as part of a conversation instead of a
line torn out of one.

## 1 What it looks like today

`msg search "dinner"` prints one line per match, `[chat] sender: body`. A hit is
whatever the needle landed in, and that is very often the least informative
message in the exchange: `sounds good`, `yeah ok`, `same place?`. What was being
answered is not shown, and neither is what came of it.

There is no way to recover it afterwards either. `msg read <chat>` prints the
*last* N messages of a conversation, and `--since` bounds it from one end only,
so a hit from two years ago cannot be reached from the chat listing at all — you
would have to page an entire history to arrive at it. The neighbourhood of an
old match is currently unreachable by any command this program has.

## 2 A window is a second query, per hit, inside the hit's chat

Search results interleave conversations, so "three messages after this one"
means three within *that hit's chat*, not the next three rows of the result
stream. Two hits in two chats have nothing to do with each other.

`fetch_messages` almost supports this already: it takes `chat_id`,
`after_rowid`, and `oldest_first`, which is a forward window. It has no backward
bound, so `FetchMessages` gains a `before_rowid` and each hit costs two small
queries — `rowid < hit` newest-first for the before half, reversed on the way
out, and `rowid > hit` oldest-first for the after half.

**Ordered by rowid, not date.** The same argument the watcher already makes:
messages that arrive out of order have the two orders disagreeing, and a window
is "what was next in the conversation", which is arrival.

**The cost is bounded by `-n`.** Default 25 hits, so at most 50 extra queries,
each an indexed rowid range restricted to one chat. That is a different shape of
query from the body scan that found the hits — no `msg_body_has`, no blob
decoding of anything outside the window — so it should disappear next to the
search itself. Measure it against the real database before believing that;
`query-performance.md` exists because a subquery that looked free was not.

**Measured, and it was not free until the query changed. (SHIPPED)** Written the
obvious way — `WHERE message.rowid < ?` with `ORDER BY message.rowid` — each
window cost about 100ms against a 763,000-message database, so `-n 25 -C 3` took
roughly 6s against 2s without context. Nothing was indexed the way the estimate
assumed: the query reads `FROM message`, so SQLite walked message rowids
downward from the hit and discarded every row belonging to another
conversation, which for a quiet chat is thousands of rows to find three.

The fix is to bound and order against `chat_message_join.message_id` whenever a
chat is named. It holds the same number, but `chat_message_join` is keyed by
(chat_id, message_id), so the same question becomes an index range scan. After
it, `-C 3` and `-C 10` both sit inside the run-to-run noise of a search with no
context at all — median 2,988ms and 2,809ms against 3,238ms — and the body scan
dominates as predicted.

## 3 Decisions

**Overlapping windows merge. (DECIDED)** Two hits four apart with `-C 5` share
most of their windows. Printing each window whole duplicates those messages and,
worse, hides that the two hits are the same stretch of conversation. Merge any
windows that overlap or touch into one run, printed once.
*Rejected:* one window per hit, which is simpler and reads as though the
conversation happened twice.

**A `--` line separates runs. (DECIDED)** grep's separator, for the same job,
and readers already know what it means.
*Rejected:* a blank line, which is ambiguous against a message whose body is
empty.

**The hit is marked. (DECIDED)** Without a marker a hit and its context render
identically, and the output stops being answerable — you can see a conversation
but not which line matched. A two-column gutter before the timestamp: `> ` on a
hit, two spaces on context. It survives being pasted, piped, and grepped again.
*Rejected:* colour or bold. This program emits no escape codes anywhere today,
and adding them here would mean the first thing it writes into a redirected file
is terminal control.

**`--with` and `--from` do not narrow the window. (DECIDED)** This is the
decision the feature stands on. `--from dana -A 2` has to mean "what Dana said,
plus what came next, whoever sent it" — filtering the window by the same person
would return Dana's *next two messages*, which is almost never the reply she
got, and it would make the feature close to pointless. The window is a slice of
the conversation, not a continuation of the filter.

The same goes for the body match, obviously — context does not contain the
needle — and for `--since`, less obviously: a window on a hit near the `--since`
boundary reaches back past it, because the bound is on what counts as a hit, not
on what may be shown around one.

**Context includes tapbacks, though search still cannot match one. (DECIDED)**
A reaction to the message beside a hit is context in the plain sense, and is
frequently the entire reply — `Liked "see you at 8"` is the answer. So the
window sets `include_tapbacks: true` while the search half keeps the default.
Note that `msg search` has no `--tapbacks` flag at all today; only `read` and
`watch` do (§4 below explains the mechanism).
*Rejected:* adding `--tapbacks` to `search` to govern both halves. It conflates
"what counts as a hit" with "what may be shown around one", and nobody wants
`Liked "…"` returned as a search result.

If the tapbacks plan lands first, this narrows rather than changes: a
reaction will already be attached to the message it reacted to, so the window
carries the reaction along with the message and this decision becomes "the window
does not suppress them".

**The prediction above was falsified when tapbacks slice 1 landed
(2026-08-09), and the record of that is worth more than the sentence.** The
build went the other way: the window *does* suppress reaction rows, because a
row beside an attached reaction is the same one printed twice, and tapbacks.md §6 had
already decided against printing the same information twice. What the reversal
trades away is the case this section's argument was actually about — a reaction
whose target sits outside the window now renders nowhere at all, where the row
once stood in for the reply. Accepted knowingly: the common case is a reaction
beside its target, where the attached rendering serves, and the distant-target case lost
its only representation in windows while keeping `--tapbacks` everywhere else.

**`-C` takes the collision with `-c`. (DECIDED)** `-c/--chat` is already taken,
so `-c 3` and `-C 3` will differ only in case and mean entirely different things
— "in the conversation matching 3" against "three messages either side". The
hazard cannot be guarded: `-c <rowid>` is a legitimate and documented way to
name a chat, so a case slip is indistinguishable from an intended use.
Taken anyway, because `-A`/`-B`/`-C` are the point — the muscle memory is the
feature, and a `-C` that is missing from the trio is a `-C` that gets typed at
some other program's expense.
*Rejected:* `--context` with no short form. Safe, and nobody would use it.

**`-n` still counts hits. (DECIDED)** Not printed messages. Otherwise `-n 25 -C
5` returns four matches and the flag stops meaning what it means in every other
command. The printed line count is then the caller's arithmetic, the same as it
is with grep.

## 4 Tapbacks, since the mechanism decides the paragraph above

A tapback is not a separate table or a flag on the message it reacts to. It is
an ordinary `message` row of its own, with `associated_message_type` set to
nonzero and a body Messages synthesizes as text — literally `Liked "after 6,
yeah"`, quoting the target. `is_tapback` on the `Message` struct is derived,
`associated_message_type != 0`, and nothing else in the program inspects the
value.

They are excluded by a single clause in `fetch_messages`:
`message.associated_message_type = 0`, added unless `include_tapbacks` is set.
`read` and `watch` expose that as `--tapbacks`; `search` never sets it and has
no flag, so a tapback can never be a search hit today. That is why §3 can turn
tapbacks on for the window without touching the search: the two halves are two
calls with two different `FetchMessages`.

What the program does **not** do is link a tapback to the message it reacts to.
`message.associated_message_guid` holds that pointer and is never read —
`threading.md §2` mentions the column only in passing, ruling out a different
one. So a tapback in a context window would render as its synthesized body,
quoting its target's text, and would not be attached to the target the way a
reply is. That was acceptable for context, and the follow-on happened: tapbacks
slice 1 reads the pointer, attaches the reaction to its target, and took the
rows out of the window — the §3 correction records the reversal.

## 5 JSON and the protocol

The reply stays a flat `Vec<Message>`. Nesting groups would be a cleaner shape
and would break every consumer of the documented one.

- `matched: bool`, defaulted true and skipped when true. A hit therefore
  serializes exactly as it does today, and only context messages carry
  `"matched": false`. A search run without `-A/-B/-C` is byte-identical to the
  current output.
- `group: Option<i64>`, present only when context was requested. A consumer
  cannot derive run boundaries itself: rowids are global, so two adjacent
  messages in one chat are not adjacent numbers, and the `--` separator would be
  unreproducible without this.

`SearchRequest` gains `before` and `after` as `Option<i64>`, both skipped when
absent, and `PROTOCOL_VERSION` bumps. The test is the one recorded in that
constant's own doc comment — builds without these fields exist and are installed
right now, so the version they speak cannot also mean the one that has them. A
new client against an older daemon must be told, not silently answered without
context, which here is the sharp case rather than the mild one: `before` and
`after` are *request* fields, so a daemon that does not know them ignores them
and answers with bare hits, and `-C 3` would quietly produce exactly what it was
asked to change.

**Shipped as 9.** This was written against 6 and warned against hard-coding 7,
correctly: `filedAs` took 7 and the `contacts` lookup took 8 while this sat in
`next`. The tapbacks plan bumps the version too and takes whatever follows.

## 6 Slices

One slice. The window, the merging, the gutter and `--`, tapbacks in the
window (since reversed — §3), the two JSON fields, and the protocol bump all
ship together — the daemon is
where search actually runs on a real machine, so a version that skips the
protocol half would only work through `--db` against a fixture.

## 7 What this is not

- **Not a `read --around <rowid>`.** The same windowed fetch would give it, and
  §1 says the gap is real, but it is a different command with its own argument
  and its own ambiguity rules. Worth doing next, on top of `before_rowid`.
- **Not threading.** A window is positional. What a message *answers* is
  `threading.md`, already shipped, and a reply inside a window will keep
  rendering its `↳ replying to` line.
- **No change to what counts as a hit.** Same body match, same person filters,
  same `--since`, same default of excluding tapbacks from the matching half.
