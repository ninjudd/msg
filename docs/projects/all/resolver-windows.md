# Plan: Resolve the person before matching chat rows

**Status:** The half that bit is fixed, on `merge-the-listing` (#36), pinned by
`rooms_cannot_crowd_a_persons_own_thread_out_of_the_answer`. What remains is
latent: it cannot bite below 5,000 chats, and this database holds 1,165.

## 1 The bug, by example

Robin has two threads: a phone thread, texted yesterday, and an email thread
last used in 2019. Robin is also in 84 unnamed rooms, and an unnamed room
renders as its members' names, so every one of those matches "robin" too.

`msg chat robin` used to start from the chat rows the name matches — 86 here —
keep only the newest 50, and then keep the person's own threads *out of that
window*. The rooms are all more recently active than a thread last used in
2019, so they filled the window, the email thread fell outside it, and the
reader answered with the phone thread alone. Nothing said so: the transcript
was silently missing every email message, and once the listing merged the two
threads into one row, the email thread's rowid was printed nowhere — not in
`msg chats`, not in `merged` — so there was no way left to reach it. Four
people on a real database of 1,165 chats were in exactly Robin's shape, with
names matching 87–112 chats each.

## 2 The fix

The right order is the obvious one: resolve the name to a person, gather every
address their Contacts record has, find the chats with each address, and answer
with all of them. `resolve_person` and `one_to_one_chats` already did the first
three; the defect was one intersection filtering their complete answer through
the windowed name-match. It is deleted — once a person resolves, their threads
are the answer, and how many rooms they are in cannot change it.

## 3 What remains, and why it can wait

The resolver still *enters* through the chat-row match: `fetch_chats` reads the
newest `NAME_SEARCH_SCAN` (5,000) rows before matching, and a spec that matches
nothing inside that window errors before the person lookup ever runs. Past
5,000 chats, someone whose every thread and room has gone quiet falls out of
reach. Demonstrated on a fixture, impossible on this database.

The full fix is the order this plan is named for: resolve the person first, and
use the chat-row match only for what is not a person — a rowid, a room named
outright, rooms found by membership, and the ambiguity error. The care it needs
is precedence: a room named exactly the spec currently stands equal to a person
of that name (`naming-a-conversation.md` §3 and §4 both hold), and reordering
must not let the person path swallow that case.

## 4 Not this

- **Not the listing.** It reads every row and merges correctly; both halves of
  this are the reader's.
- **Not ambiguity handling.** A name that names several people errors, and
  should. The names here name one person each.
