# Plan: Resolve the person before matching chat rows

**Status:** Shipped 2026-08-09, in two slices. The windowed intersection came
off on `merge-the-listing` (#36), and the person-first order this plan is named
for landed on `person-first-resolution` (#37, protocol 14). What §3 still records
is the residue for what is *not* a person — a room's own name or an address
fragment past the newest 5,000 chat rows — which is loud, cannot bite below
5,000 chats, and this database holds 1,165.

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

The second slice (#37) makes that order the entry itself. `resolve_conversation`
resolves the person before any chat row is matched, so the window cannot even
decide *who* a spec means. An address names the person too, whole or as a
fragment — the whole conversation, led by the address's own thread so the send
target stays where it was aimed; a whole address leads on its key and a
fragment by the substring rule every other fragment uses. A name two contacts
share errors naming the people, an address alongside each — unless a room's own
label is exactly the string typed, which is the one claim that outranks that
error. Chat rows are matched as text only for what no contact claims: a room's
own name, a group identifier, an address fragment. A room labelled exactly the
string typed wins outright — unless somebody is *named* exactly it too, and
then the two whole claims are the tie this reports. The axis is whether another
claim on the string is whole, never how many people answer to part of it; the
label check runs against the whole table instead of the window. This sentence
has now been corrected twice — first hung on ambiguity, then on the count of
people — and both times the rule underneath was the same one: whole beats
fragment, and wholes tie.

## 3 What remains, and why it can wait

Both halves this section used to record — the loud error before the person
lookup ran, and the silent half-conversation when one thread still matched —
died with #37, because for a person the chat-row match no longer runs at all.

What is left is the window on what the text match still serves: `fetch_chats`
reads the newest `NAME_SEARCH_SCAN` (5,000) rows, so past 5,000 chats a
long-quiet *room* stops being findable by its name, and a contactless address
fragment stops reaching a long-quiet thread. One member of the residue is a
person after all: someone with no thread of their own — you only ever share
rooms — resolves and then finds nothing, so they drop to the same text match
by design, and past 5,000 chats their quiet rooms are out of reach by their
name too. Loud in all three cases — "no chat matching", with the rowid still
working — and rooms never merge, so the silent half-conversation mode has no
equivalent here. Impossible below 5,000 chats.

## 4 Not this

- **Not the listing.** It reads every row and merges correctly; both halves of
  this are the reader's.
- **Not ambiguity handling.** A name that names several people errors, and
  should. The names here name one person each.
