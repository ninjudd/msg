# Plan: One person, one conversation

**Status:** Not started. The identity half already shipped — `resolve_chat`
collapses several conversations with one person down to one answer — but it
answers with the most recently active and the rest stay invisible. §9 lists two
things to verify against the database before building on this.

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

## 4 A merged transcript is one query, not two

The obvious implementation — fetch each thread, interleave the results — is not
necessary, because `chat_message_join.message_id` *is* `message.rowid`. The
join is on equality, so the number that orders messages within one chat is the
same number that orders them across all of them.

So merging is `chat_id IN (…)` where the code says `chat_id = ?`, and the
ordering, the `before_rowid`/`after_rowid` bounds, and the `oldest_first`
handling all keep working unchanged. `fetch_messages` takes a list of chat ids
instead of an `Option<i64>`.

**The index optimisation survives, which is the part worth checking rather than
assuming.** `search-context.md §2` records that bounding against
`chat_message_join.message_id` turned each context window from a ~100ms scan
into an index range scan, because `chat_message_join` is keyed by
(chat_id, message_id). `chat_id IN (a, b) AND message_id < ?` is two range
scans over that same index rather than one, which should be the same shape at
twice the count. Should be. Measure it on a merged conversation with `-C 10`
before believing it, because that plan exists as a record of an estimate like
this one being wrong.

**Arrival order, not date order.** Same argument the watcher and the context
windows already make: rowid is arrival, date is what the sender's clock said,
and the two disagree for messages that arrive out of order. Merging by date
would reorder a conversation against itself for exactly the messages most
likely to be interesting.

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

**The merged conversation is named for the person, and the send target is named
separately. (DECIDED)** `describe_target` prints `Name (address)` for a
one-to-one, and a merged conversation has more than one address, so the header
is the person's name alone. But `send --dry-run` must keep printing the single
address it would actually send to — the whole safety value of that flag is that
it names the real destination, and "would send to Dana" is exactly the sentence
that hides a send to the wrong one of her two numbers.

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

So this plan does not touch the send path at all. It is worth saying explicitly
because "merge their conversations" sounds like it should also mean "and pick
the address for me", and the existing answer to that is already the one we
want — the merge changes what is *shown*, never where a message goes.

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

**Can one message belong to two chats?** `MESSAGE_FROM` joins
`chat_message_join`, so if a message is joined to both merged chats it produces
two rows and appears twice in the transcript. Within a single chat that cannot
happen; across a merged set it depends on whether Messages ever writes a message
into more than one conversation. Check it directly before deciding whether the
query needs a `DISTINCT` or a dedupe by rowid — and if the answer is no, do not
add one, since a guard against a case that does not occur is exactly what
`AGENTS.md` says not to ship.

**Does `chat_id IN (…)` keep the index?** §4 argues it does. `EXPLAIN QUERY PLAN`
answers it in one command, and a `-C 10` search across a merged conversation
answers it in the way that matters.

## 10 Slices

**One: `read` merges.** `fetch_messages` takes chat ids, `resolve_chat` returns
the set rather than the winner, `ReadReply` gains `merged`, the protocol bumps.
This is the reported bug and it is coherent on its own.

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
