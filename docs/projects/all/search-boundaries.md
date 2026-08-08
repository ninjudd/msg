# Plan: Stop matching in the middle of a word

**Status:** Not started. Written from an observed case: a three-letter needle
returned two hits that were not the word at all, buried inside longer words, and
one that was. §5 is an open question that has to be answered before this ships.

**Goal:** Make a search needle match at the start of a word, so a short one
stops finding itself inside unrelated longer words.

## 1 What it does today

`msg_body_has` is a plain case-folded substring test — `contains_ignoring_case`,
and nothing about it knows what a word is. So a three-letter needle finds every
word that happens to contain those letters in that order. Searching `art`
returns the message that says `is that art deco or just old`, and alongside it
`we started at six` and `the apartment above ours`.

Two of those three are noise, and the shorter the needle the worse the ratio
gets. It is at its worst for exactly the search that most wants to work: an
acronym or an initialism, which is short by construction and which the user
knows is a word rather than a fragment.

This is not a case of the needle being too vague to answer. `art` is a perfectly
specific thing to look for; the program is answering a different question from
the one asked, and it will answer it the same way however many results are
returned. `-n` does not help, and neither does adding context around the hit —
`search-context.md` makes each of these noise hits *longer*, which is the wrong
direction if two thirds of them should not be there.

## 2 The rule: a hit starts where a word starts

A match qualifies when the character immediately before it is not alphanumeric,
or when the match is at the very start of the body. Nothing is asserted about
where the match *ends*.

That asymmetry is the whole design and is worth being explicit about, because
the symmetric rule is the one that first suggests itself. Requiring both ends to
sit on a boundary means whole-word matching, and **`start` must still match
`starting`** — typing a prefix and letting the tail fall where it may is what a
search box is for, and a rule that breaks it has traded one wrong answer for
another. Requiring only the start keeps that working while removing every hit of
the kind §1 shows, because an interior match is precisely one that does not
begin at a word start.

So the three cases a test should pin, in one line each: `start` matches
`starting`, and `art` matches neither `started` nor `apartment` but does match
`art deco`.

## 3 It has to run on the decoded body, not the blob

`register_body_match` runs the same predicate twice, deliberately: once in SQL
over the raw `attributedBody` blob as a prefilter, and once over the decoded
body afterwards. Its doc comment turns on the prefilter being a *superset* of
the decoded filter — it may over-match, it may never under-match.

**A boundary test on the raw blob breaks that.** In a typedstream the text is
introduced by its length, and `read_int` returns any byte that is not a width
marker as the length itself — so `decode_attributed_body` starts the text at the
byte immediately after it. For a body shorter than 0x80 that byte *is* the
length, and lengths are arbitrary: a body of exactly 100 characters is preceded
by `0x64`, the letter `d`. A 100-character message beginning `art …` therefore
sits in the blob as the bytes `dart …`, and a boundary test reading the blob
sees a letter before the needle and rejects a real match. Longer bodies take a
width marker and a little-endian length whose last byte lands in the same place;
that byte is usually a small high byte rather than a letter, which makes the
failure rarer but not rarer in a way anything should depend on.

So the prefilter keeps the substring test exactly as it is, and the boundary
rule goes in the decoded filter alone — the single `messages.retain` in
`fetch_messages`. The comment above it currently says the two predicates are
deliberately identical so they cannot disagree; it becomes the superset
statement instead, which is what `register_body_match` already documents.

## 4 What counts as a word character

**Alphanumeric by Unicode, via `char::is_alphanumeric`. (DECIDED)** Not
`is_ascii_alphanumeric`, which would treat every accented letter as a boundary
and let `art` match `Beçart`. No dependency: this is in `core`.

**The test is skipped when the needle does not start with a word character.
(DECIDED)** A needle of `😂` or `?!` or `://` has no word start to sit on, and
applying the rule to it would reject `lol😂` for the needle `😂` — the character
before it is a letter, and there is nothing wrong with that match. If the first
character of the needle is not alphanumeric, the search is not word-shaped and
the plain substring test is correct.
*Rejected:* applying the rule uniformly, which silently breaks emoji and
punctuation searches for no benefit.

**Scripts written without spaces need a carve-out. (DECIDED, unimplemented)**
Han, Hiragana, Katakana, and Hangul characters are alphanumeric, and text in
those scripts is written without spaces between words. The rule as stated would
therefore reject nearly every match in a Chinese or Japanese message — a
regression from working search to almost none, which is far worse than the noise
this plan removes. When the needle's first character is in one of those blocks,
skip the boundary test. That is a handful of range comparisons written by hand,
in the spirit of the rest of this program.
*Rejected:* real word segmentation, which needs a dictionary and is not
something this program is going to carry.

This is the part most likely to be got wrong quietly, because it only misbehaves
for text the author does not send. Whether it needs a test with CJK fixture
messages, or whether the range check is self-evident enough to go without, is a
judgement call at implementation time — but the check itself is not optional.

## 5 Should there be a way back to substring matching? (OPEN)

An escape hatch — `--substring`, or grep's `-w` inverted — would restore today's
behaviour for the cases that genuinely want an interior match: the last four
digits of a phone number, a fragment of a URL, part of a long identifier.

Not proposed here, on the repository's own rule that a fix for a case nobody has
hit is worse than no fix. No such search has been observed, the flag is trivial
to add later, and adding it now means shipping an untested code path plus a line
of `--help` that implies a problem exists.

**This is the decision that needs an answer before implementation**, because it
also settles whether §2's rule is the default or an opt-in. Shipping the rule as
the default with no flag is the recommendation; shipping it behind a flag would
leave the observed bug in place for everyone who does not know to type the flag,
which is everyone.

## 6 It makes the widening loop work harder

`fetch_messages` over-fetches and then trims, because the SQL `LIMIT` bounds raw
prefilter matches while the decoded filter drops some of them. When it drops too
many it re-asks for four times as many rows and scans again.

This plan increases the drop rate by construction — every interior match is now
a prefilter hit that the decoded filter rejects — so needles that are common
inside words and rare at word starts are the new worst case. `ing` is the
obvious one: overwhelmingly a suffix, so nearly every candidate row would be
fetched, decoded, and thrown away, and the loop could go 100 → 400 → 1600 with a
full body scan each pass before `exhausted` stops it.

Worth measuring rather than guessing — `query-performance.md` exists because a
subquery that looked free was not. If it bites, the fix is not to weaken the
rule but to raise the first ask when the needle is short, since needle length is
known before the query runs and short needles are exactly the ones that pay.

## 7 Scope: message bodies only

Chat and contact name matching is a separate substring test — `answers_to` on
`Contact`, and the `displayName`/`chatIdentifier`/`handles` clause in
`matching_chats`. Those already have exact-match precedence in front of them,
which is what settled the case that prompted the nickname work, and the noise
this plan removes has not been observed there. Leave them alone until it is.

If it ever does come up, the rule transfers unchanged and the two should share
one predicate rather than growing a second definition of what a word start is.

## 8 What this is not

- **Not stemming or fuzzy matching.** `run` does not find `running`, and a typo
  finds nothing. Both are real features and neither is this one.
- **Not a search index.** `search-index.md` covers what indexing bodies would
  cost and why it is deliberately not being built; this changes the predicate,
  not the scan.
- **Not a change to which messages are eligible.** Same person filters, same
  `--since`, same exclusion of tapbacks from the matching half. It narrows what
  counts as a hit within a body, and touches nothing else.
- **Not a change to context windows.** `search-context.md §7` closes by saying it
  makes no change to what counts as a hit; this plan is the one that does, and
  the window logic sits downstream of it either way.
