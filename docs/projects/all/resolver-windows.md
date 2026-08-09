# Plan: The reader bounds what it searches and the listing does not

**Status:** Not started. Found while building `conversation-merging.md` slice
two, which is what made it visible rather than what caused it. Both bounds
predate that branch.

**Goal:** Make `msg chat <name>` reach the same conversations `msg chats` shows,
so a name that is listed can be opened and opens all of it.

## 1 Two bounds, one shape

`fetch_conversations` reads every chat, because merging means a count of threads
is not a count of conversations until the merge has run. The reader still reads
a window. There are two:

- **`CHAT_MATCH_SCAN`, 50.** `resolve_conversation` matches chat rows by name and
  keeps the newest 50 before intersecting with the person's own threads.
- **`NAME_SEARCH_SCAN`, 5,000.** `fetch_chats` reads that many rows before
  matching a query in Rust.

Neither is new. What is new is that the listing has no equivalent, so where the
two used to be wrong together they are now wrong apart — and a conversation that
is listed but cannot be opened is worse than one missing from both, because the
listing is where rowids come from.

## 2 The 50 is the one that bites today

Measured on a real database of 1,165 chats. Four people have two threads each
that `msg chats` merges and `msg chat <name>` does not, and the cause is not
ambiguity: the name resolves to exactly one person, by §8's rule that an exact
match breaks a tie among people sharing a first name.

It is the intersection that loses the thread. Those names match 87, 110, 112 and
71 chats, and **84 of the 87 are groups that person is in**. The window keeps the
50 most recent of those, the person's own older low-traffic thread falls outside
it, and `matches ∩ theirs` never sees it. So the reader answers with one thread
and no indication that it is half a conversation — which is the defect
`conversation-merging.md` §5 exists to remove, surviving in the read path.

The intersection is the wrong place for a window in any case. It is bounding the
candidate *chat rows* when what the answer needs is the *person's* threads, and
`one_to_one_chats` already returns those directly without a limit.

## 3 The 5,000 is latent

Past 5,000 conversations a name reaches the listing and not the reader. Not
reproducible on this database; demonstrated on a fixture. `chats_by_id`'s comment
records the same trap on the same table and is the precedent for not trusting a
generous number.

## 4 Open

**What replaces the 50?** Dropping the truncation makes the reader read the whole
chat table on every `msg chat <name>`, which measured at the same 0.11s the
listing already pays. It also widens the ambiguity error from "at least 50 chats
match" to the true count, which is a better message and a changed one.

**Does the intersection need `matches` at all?** If the person resolves, their
threads are `one_to_one_chats`, and filtering those by which chat rows happened
to match the spec is what drops the thread. Removing the intersection may be the
whole fix, and is smaller than removing either window.

## 5 Not this

- **Not the listing.** It merges correctly; this is the reader disagreeing.
- **Not ambiguity handling.** A name that names several people errors, and
  should. These names name one.
