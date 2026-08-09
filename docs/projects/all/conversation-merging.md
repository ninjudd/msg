# Plan: One person, one conversation

**Status:** Shipped. Slice one in #30 — `msg chat <name>` merges every
conversation with a person, `msg chat <rowid>` reads one thread, and the reply
carries `merged`. Slice two in #36 collapses the `msg chats` listing on the same
rule, at protocol 13: measured on a real database, 648 one-to-one threads became 602
conversations, and the 517 groups were untouched.

Two things slice two settled that this plan did not ask about. The limit counts
conversations, so it can no longer be given to SQL — which costs nothing,
because the aggregate `CHATS_SQL` joins against was never correlated and always
ran in full. And the query is matched after the merge rather than before, so a
search still reaches someone by an address their newest thread does not use.

Two things it surfaced. The listing no longer shows the rowid of a thread that
is not the leading one, and §5 promised the listing as where rowids come from;
they are reachable in `merged` from `msg chat <name> --json`, and whether the
listing should carry them too is open. That reachability was briefly untrue:
the reader kept only the newest 50 name matches before answering, so for four
people the listing merged a second thread that `msg chat` then missed — older
than this slice, made visible by it, caught in review. The window came off that
path on this branch, and the bound that remains — latent past 5,000 chats — is
written up as `resolver-windows.md`.

The command was `msg read` when this was written and `ReadReply` was the reply's
name; #35 renamed both, and protocol 10 became 13 by way of 12. The body below
is left in the vocabulary of its time.

Two of §4's claims were wrong and are corrected in place, both about
performance rather than behaviour: `chat_id IN (…)` keeps the index and loses
the ordering, so the merge fetches one thread at a time; and the transcript is
ordered by date rather than by arrival, because that is what `read` already
meant. §9's index question is answered. The duplicate-rowid question is still
open and remains a measurement rather than a blocker — the merge dedupes on
rowid either way, which the tree already had to do three times elsewhere.

**Goal:** When someone is reachable at more than one address, show their
messages as a single conversation, the way Messages does, instead of picking
one thread and silently hiding the others.

## 1 What it does today

Messages keeps a conversation per address. Someone with a phone number and an
email address has two, and until recently `msg read <their name>` matched both
and asked which was meant — a question nobody can answer, since it is the same
person either way.

That got fixed by collapsing: `resolve_chat` checks whether every candidate is
the same person, and if so returns the one last active. The ambiguity is gone
and the command works. But it works by *choosing*, and everything in the thread
it did not choose is unreachable — not merged, not mentioned, just absent. A
conversation that moved from email to phone three years ago reads as though it
started three years ago.

## 2 What it costs, measured

Against a real database, counting one-to-one conversations and grouping them by
the name they display as:

| | |
| --- | --- |
| One-to-one conversations | 648 |
| Distinct people | 602 |
| People with more than one conversation | 42 — 38 with two, 4 with three |
| Of those, a phone/email mix | 34 |
| Messages in the threads not shown | 14,479 |
| People whose *hidden* thread is the larger one | 9 |

The last row is the one that matters. For nine people, `msg read <name>` shows
the smaller half of the conversation and hides the bigger one, and nothing in
the output says so.

Two caveats on these numbers. They group by display name as a proxy for the
Contacts record that §3 actually keys on, so two people sharing a name are
counted here as one — the real clustering is a little smaller. And 63% of all
one-to-one messages sit in conversations that are split, which sounds
implausible until you notice it is the expected shape: the people you have both
a phone and an email thread with are the people you have talked to for long
enough to have both.

## 3 "The same person" is already decided, and already shared

`person_identity` is the rule: the Contacts record where there is one, the
normalized handle otherwise. `sole_person` applies it to a conversation and
refuses to speak for a group. Both exist, both are used by `resolve_chat`, and
this plan needs no new definition — which is the point of them being shared. A
merge that disagreed with the disambiguation about who someone is would be a
subtle and very confusing bug.

The record rather than the name it renders as, because two records can
legitimately carry one name, and merging on the rendered name would splice a
stranger's messages into the transcript. That is the worst failure this feature
can have, and it is already ruled out by construction.

## 4 One order across two threads, fetched one thread at a time

Two threads are one transcript because `chat_message_join.message_id` *is*
`message.rowid` — the join is on equality — so a message has one identity in
whichever thread it is reached through. That is what makes interleaving
coherent, and what lets the merge recognise a message that arrives from both
fetches as one message rather than two.

**Ordered by date, which is what `read` already meant. (CORRECTED)** This
section first said the merge should order by arrival, on the grounds that the
watcher and the context windows do. They do — and they do it by setting
`oldest_first` or `before_rowid`, which is what selects rowid ordering in
`fetch_messages`. `read` sets neither, so it has always taken
`ORDER BY message.date DESC` and shown a transcript in clock order. Generalising
from the two callers that order by arrival to the one that does not was the
error.

It matters because the merge cannot quietly mean something different from the
command it is part of. Against a one-chat fixture where the newest arrival
carries the oldest clock time, `msg read` prints it first:

```
Jan 13, 4:26 AM  +13105551234: LATE ARRIVAL, clock says first
Jan 13, 4:27 AM  +13105551234: clock says second
Jan 13, 4:28 AM  +13105551234: clock says third
```

Sorting the merge by arrival would print that message last, so the same messages
would read in one order for somebody with a single conversation and in another
for somebody with two. Date, with rowid only to break a tie.

There is a second reason, which is a correctness one rather than a consistency
one: each thread is *selected* with `ORDER BY message.date DESC`, so trimming
the union by anything else can drop a message a thread returned and keep one it
never offered. The fetch and the trim have to agree, and the fetch is date.

**But the query stays one chat at a time. (CORRECTED)** This section first said
the merge was `chat_id IN (…)` in place of `chat_id = ?`, with the ordering and
the rowid bounds carrying over unchanged, and told an implementer to measure
that before believing it. Measured, it is wrong, and in the way
`search-context.md §2` is a record of.

`IN` keeps the index and loses the ordering. SQLite walks two ranges of
`chat_message_join` and they are individually ordered but not jointly, so
`ORDER BY message_id DESC` becomes `USE TEMP B-TREE FOR ORDER BY` — a sort over
every message in the conversation, before the `LIMIT` applies. The cost
therefore scales with how much the two people have said to each other rather
than with how much was asked for. Against a synthetic database with the real
schema and its `(chat_id, message_id)` primary key, `LIMIT 50` newest-first:

| Conversation size | `chat_id = ?` | `chat_id IN (a, b)` | per chat, then merge |
| --- | --- | --- | --- |
| 15,000 messages | under 1ms | 10ms | under 1ms |
| 300,000 messages | under 1ms | 70–80ms | under 1ms |

So the shape this section originally dismissed is the right one: fetch each
thread with the bounds and limit it already takes, then merge. Each fetch stays
on the index walk that terminates early, and the merge is a bounded number of
rows — at most `limit` per thread — sorted by rowid in Rust. `fetch_messages`
keeps its `Option<i64>`, and a caller above it merges, which is also less
invasive than widening it.

The merge is where a duplicate rowid is dropped, if §9's question turns out to
need that.

**Confirmed on the real database. (SHIPPED)** Against the largest split
conversation there, 148,738 messages across its threads, a merged `-n 50` read
costs about one extra thread's fetch over reading a single thread — 600-720ms
against 450-630ms end to end through the daemon, most of which is fixed
overhead — and `-n 500` is the same, which is the flatness the per-thread shape
predicts and the `IN` shape would not have.

## 5 Decisions

**A name means the person; a rowid means the thread. (DECIDED)** This is the
rule the whole feature hangs on. `msg read <name>` merges every conversation
with that person. `msg read --chat <rowid>` shows that one thread alone, merging
nothing. So the merged view is what you get by default, and the unmerged view
stays reachable and stays exact — which matters because the rowid is how you
address a thread when the merge is wrong.
*Rejected:* a `--no-merge` flag. It answers the same need with a second way to
say a thing that can already be said, and `--chat` has to keep working anyway.

**The listing merges too. (DECIDED)** `msg chats` showing 648 rows for 602
people, with the same name twice and no indication why, is the same defect one
level up — and it is where rowids come from, so it is where someone goes to find
the thread they want to address. A merged row carries the combined message
count, the most recent activity across all of it, and the rowid a send would go
to.
*Rejected:* merging `read` but not the listing. The counts would then disagree
with each other, which is worse than either behaviour on its own.

**A filtered thread does not merge into an unfiltered one. (DECIDED)**
`chat.is_filtered` is Messages' Unknown Senders bucket. If one address of a
person is filtered and another is not, merging silently promotes filtered
content into a conversation the user considers known. Keep filtered threads out
of the merge unless `--unknown` is passed, which is what that flag already means
everywhere else.

**Which cuts both ways, and that is the easy half to miss. (CORRECTED)** The
leading thread is kept whatever it is, because naming a conversation reaches it
even when Messages filters it and because dropping it would move the send
target §7 promises not to move. Filtering only the *rest* around it leaves the
rule holding exactly when the filtered thread happens to be the older one:
reverse the activity order and a plain `msg read` merges Unknown Senders content
into a known conversation. So when the leading thread is filtered and
`--unknown` was not passed, nothing merges into it at all — which is precisely
what the command did before merging existed.

**The merged conversation is named for the person, and the send target is named
separately. (DECIDED, and already true)** Nothing needs building for this, which
is worth recording so nobody builds it. `msg read` prints `reply.chat.name` —
the contact's display name, already the person alone. The address is added only
by `describe_target`, whose sole callers are the two send paths: the CLI's
`--dry-run` line and the daemon's send confirmation, the latter commented "so
the confirmation names which of a person's conversations this went to".

So the header and the send description were never the same call, and the
separation this decision asks for is a property of the code rather than a change
to it. The thing to avoid is teaching `describe_target` about merged chats or
giving it a parameter: that edits a function whose only callers are send paths
in order to fix a header it does not produce, and `AGENTS.md` treats a send as a
production write. `send --dry-run` must keep naming the single address it would
actually reach — "would send to Dana" is exactly the sentence that hides a send
to the wrong one of her two numbers — and it keeps doing so by being left
alone.

**Groups never merge. (DECIDED)** `sole_person` returns `None` for a group and
that propagates. Two rooms with the same membership are two rooms.

## 6 The reply shape and the protocol

`ReadReply` is `{ chat: Chat, messages: Vec<Message> }`, and a merged
conversation is not one `Chat`. Rather than replace it:

- `chat` stays, and stays the thread a send would go to — the most recently
  active. Every existing consumer keeps reading the field it reads today, with
  the same meaning it has today.
- `merged: Vec<i64>`, the rowids of the other threads, skipped when empty. An
  unmerged conversation therefore serializes byte-identically to what it does
  now.

That is the shape `search-context.md §5` used for `matched` and `group`, for the
same reason: the flat reply is documented and has consumers, and a nested one
would be cleaner and would break them.

`PROTOCOL_VERSION` bumps, and the reason is the sharp one rather than the mild
one. Merging is a *server-side* behaviour change: a new client asking an old
daemon to read a person gets a single thread back and no field to tell it that
is what happened. The answer looks correct and is missing half the conversation,
which is precisely the failure the version constant exists to prevent. It is 9
today; whatever the tapbacks plan and this one do, neither may hard-code the
next number.

## 7 Sending is unchanged, deliberately

`send` resolves through the same `resolve_chat` and therefore already picks the
most recently active thread for a person. That is the right behaviour and
matches Messages, which replies on the address the conversation last used.

So this plan does not touch the send path at all, and that holds exactly rather
than approximately: §5 records that the transcript header and the send
description were never the same call, so nothing in the display half reaches
into `describe_target` or anything downstream of it. It is worth saying
explicitly because "merge their conversations" sounds like it should also mean
"and pick the address for me", and the existing answer to that is already the
one we want — the merge changes what is *shown*, never where a message goes.

An address is how you choose where it goes, and slice two had to defend that
rather than restate it: once the reader can answer an address with the person's
folded threads, the typed address's own thread stays at the head of the answer,
and the leading thread is the send target. So the rule §5 states in two halves
has a third case sitting across them — a name means the person, a rowid means
the thread, and an address means the person to *read* and its own thread to
*send to*.

## 8 What the listing shows

One row per person, with the combined count and the latest activity. The rowid
shown is the send target's, so copying it and passing it to `--chat` gives the
thread a reply would go to rather than an arbitrary member of the group.

Whether a merged row should also show how many threads it merges — `Dana (2)` or
similar — is left to implementation. It is a display question with no behaviour
behind it, and it will be obvious which reads better against real output.

## 9 Verify these two before building

Both are the kind of assumption this repository has already been wrong about
once.

**Does Messages join one message to two one-to-one chats of the same person?**
Not whether a message can be in two chats at all — that is settled, and the tree
already handles it three times. `MESSAGE_FROM` joins `chat_message_join`, so
such a message comes back as two rows with one rowid;
`a_message_in_two_chats_shows_its_attachment_in_both` constructs exactly that
and asserts two copies, `attachments_for` dedupes for it, and the two `.cloned()`
calls in `fetch_messages` exist because `remove` would hand the attachments and
the reply quote to whichever row arrived first.

So a dedupe here would be the fourth instance of handling a known case, not a
guard against an imagined one. What is unmeasured is the narrower question
above: the existing test joins two chats by hand, and nothing establishes that
Messages does it for two conversations that resolve to one person.

Worth knowing why the merge is where this first bites. Today `chat_id = ?`
restricts to a single join row, so `read <person>` has never been able to
produce a duplicate — only an unscoped fetch can, which is why the existing
handling lives in the attachment and reply paths. `chat_id IN (a, b)` is exactly
what opens the read path to it. If the merged query turns out not to need a
dedupe, say so as a difference from those three cases rather than as an absence
of the case.

**(MEASURED — no such join was found, so the merge is a difference from those
three cases.)** Slice two gave a way to ask without writing SQL: the listing
counts `chat_message_join` rows and `chat` dedupes by rowid, so one message in
two of a person's threads shows up as the listing counting higher than `chat`
prints. Across the 30 merged conversations under 4,000 messages, 29 agreed
exactly. So the read path's `dedup_by_key` is not currently earning its place on
this database, and it stays anyway: it costs one pass over a sorted vector, the
three existing cases prove Messages does join one message to two chats, and §4
now issues `chat_id = ?` per thread rather than `chat_id IN (…)`, which narrows
where a duplicate could enter without closing it.

The thirtieth is worth recording as what it is rather than rounding off. One
conversation counts 14 and prints 12, and the two threads are not the cause —
its leading thread alone counts 14 and prints 12, so whatever the gap is, it is
inside a single thread and predates merging. Orphaned join rows would produce
exactly this, since the count reads `chat_message_join` and the fetch joins
`message`. Not measured, and not this plan's question.

**Does `chat_id IN (…)` keep the index? (ANSWERED — it keeps the index and
loses the ordering.)** `EXPLAIN QUERY PLAN` reports the same covering-index
search plus `USE TEMP B-TREE FOR ORDER BY`, and the timings are in §4. The
answer is why §4 now fetches one thread at a time, so this question is settled
rather than outstanding.

## 10 Slices

**One: `read` merges.** A resolver returns the set rather than the winner, a
caller above `fetch_messages` fetches each thread and merges by rowid,
`ReadReply` gains `merged`, the protocol bumps. `fetch_messages` itself is
unchanged, per §4. This is the reported bug and it is coherent on its own.

**Two: the listing merges.** `msg chats` collapses rows. Separable because
nothing in slice one depends on it, and it is the slice most likely to want
adjusting once there is real output to look at.

Search needs nothing in either slice: `--with` and `--from` already span every
address a person has, because they resolve through the same identity rule.

## 11 What this is not

- **Not a change to who someone is.** §3 reuses `person_identity` unchanged. If
  the merge is ever wrong about two people being one, the bug is in the Contacts
  record, not here.
- **Not threading.** A merged conversation is still a flat transcript in arrival
  order. What a message replies to is `threading.md`, already shipped.
- **Not a rewrite of the database.** Messages keeps its per-address
  conversations and so does this; the merge exists only in what `msg` prints.
- **Not deduplication of message content.** If the same words were sent to two
  addresses they are two messages and both appear.
