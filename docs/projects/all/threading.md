---
status: Shipped
---

# Plan: Make replies read as replies

**Status:** Done. Slice 1 shipped — a reply says what it is answering, in
rendered output and in `--json`. §5 slice 2 is deliberately not being built, and
§8 records why and what would be worth building instead. Verified on the real
database: 164 replies resolved across 9,656 messages in 30 conversations, and no
excerpt left carrying a placeholder.

**Goal:** Stop an inline reply reading as an unrelated remark that happens to
come later.

## 1 What it looks like today

Messages has had inline replies since iOS 14: you reply to one message and it
renders attached to it, out of the flow. `msg read` prints strict chronology, so
a reply lands wherever its timestamp puts it — often many messages after what it
answers, next to a conversation that has moved on. The reader has no way to tell
that "yes, that works" is answering something from twenty minutes ago.

## 2 Two columns, and the tempting one is wrong

Read out of the daemon's log against the real database. DDL and aggregates only,
no rows.

`message` carries **both** `reply_to_guid` and `thread_originator_guid`, and the
first is far more common — which is exactly the trap.

| Measured | Count |
| --- | --- |
| Messages | 757,842 |
| `reply_to_guid` set | 117,577 |
| ... of those, with no `thread_originator_guid` | 115,304 |
| `thread_originator_guid` set | 5,649 |
| Distinct threads | 3,742 |

**`reply_to_guid` is not the user's reply.** It is set on 15% of every message in
the database, which no one's conversations look like, and 115,304 of those rows
have no thread at all. It resolves to a real message almost always (117,452 of
117,577) and equals `associated_message_guid` only 71 times, so it is not
tapback bookkeeping either. Whatever it records — delivery chaining, most
likely — it is not "this person replied to that message", and building on it
would thread 15% of history at random.

**`thread_originator_guid` is.** 0.75% of messages, which is what a deliberate
feature looks like. It always comes with a `thread_originator_part` — 5,649 of
5,649, every one containing exactly two colons, so it is a structured reference
to *which part* of a multi-part message was replied to. And it has its own index,
`message_idx_thread_originator_guid`, which is the clearest tell available: Apple
queries this column and does not query the other one.

## 3 What the shape says about the design

**Threads are shallow.** Of 3,742 threads, 2,597 have one reply, 774 have two,
224 have three. The long tail is thin. Nothing here needs a tree; a reply
pointing at its originator is the whole structure.

**An originator can be missing.** 5,646 of 5,649 resolve to a message that still
exists. The other 3 point at something deleted, so a reply whose originator has
gone must still render as a reply rather than disappear or crash.

**A reply is not always in its originator's conversation.** 19 of them are in a
different chat, which sounds impossible until you remember a message can be in
two conversations. So the lookup cannot be scoped to the current chat.

**One reply in eight is a tapback.** 706 of 5,649 have a non-zero
`associated_message_type`. Those are already hidden unless `--tapbacks` is
passed, and they should stay hidden — a reaction placed in a thread is still a
reaction. It does mean the count of visible replies is smaller than the column
suggests.

## 4 What to show

The originator's rowid, its sender, and a short excerpt of it — enough to
recognise which message is being answered without reprinting it. The excerpt has
to be built from the same decoded body everything else uses, so an attachment in
the originator shows as its description rather than as an invisible character.

Rendered, a reply says what it answers before saying itself. In `--json` the same
thing arrives structured, so a consumer can rebuild the thread rather than parse
prose.

## 5 Slices

1. **Say what a reply is answering.** One extra lookup per page of messages,
   keyed on `guid`, which is unique and therefore indexed. Chronology is
   untouched.
2. **Group a conversation by thread**, so a thread reads together instead of
   scattered through the transcript. Deliberately not started: it changes what
   `msg read` means, and slice 1 is what makes the case for it or against it.

## 6 What this is not

Not editing or unsending, which need private APIs and are listed as limitations
in the README. Not `associated_message_type`, which is tapbacks and already
handled. A reply is an ordinary message that points at another one.

## 7 The excerpt had the bug this program already fixed once

Caught on the real database rather than in a test. The first version built the
excerpt from `message_body`, the raw decoded body — so a reply to a photo quoted
a bare U+FFFC, which is exactly the hole [attachments.md](attachments.md) §1
exists to close, reintroduced one layer up.

§4 above had already said not to do this, which is the useful part: the plan was
right and the code did not follow it, and only real data showed the difference.
The excerpt now runs the same `describe_attachments` the transcript does, so a
quoted photo reads as `[#76521 …m4a, 3.7 MB]` rather than as nothing at all.

That costs one more batched lookup, over the originators only — a few rows, on a
population that is 0.75% of messages to begin with.

## 8 Slice 2 is not being built, and what might be instead

§5 slice 2 was regrouping a conversation so a thread reads together. It is not
being built, and slice 1 is the reason: it did not merely precede it, it took
most of the case for it away.

**Threads are shallow enough that there is little to group.** 2,597 of 3,742
threads have exactly one reply. Grouping those moves one message next to one
other message — and the quote now prints inline, so the connection is already
readable without moving anything.

**Reordering costs the one property `msg read` can be relied on for.** Time
order is not decoration: `--since` bounds it, `watch` extends it, and every
`--json` consumer assumes it. Trading that for an adjacency the quote already
supplies is a bad exchange.

**What is worth building instead**, if anything: not reordering the transcript,
but showing one thread on demand — every message in it, given **any** message in
it rather than only the originator. That is the right shape because a reader
finds a reply, not a root: resolve the given message to its thread first (its own
`thread_originator_guid`, or its own guid if it is the originator), then take
everything sharing it. Chronology elsewhere is untouched, because nothing else
changes.

**The obstacle is getting the id, and it is the one attachments already solved.**
Rendered output prints no rowid — `--json` has them, but a person reading a
transcript has nothing to type. Attachments met exactly this and answered it by
printing the id in the description, which is what made `msg save` usable at all
([attachments.md §4](attachments.md)). The same answer fits here and costs
almost nothing: the `↳ replying to …` line is the one line that exists *only* on
replies, so putting the originator's rowid in it adds nothing to the 99% of
messages that are not replies, and a thread would announce its own handle
wherever one is visible.

That is the piece to build first if this is ever picked up — the command is
straightforward and unusable without it.
