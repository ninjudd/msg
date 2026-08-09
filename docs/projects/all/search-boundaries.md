# Plan: Stop matching in the middle of a word

**Status:** Shipped 2026-08-09 — names first, then the body half in #40 at
protocol 15. The rule runs in the decoded filter, placed as §3 asked, reading
the boundary from the unfolded text; §5 is decided as the default with no
flag. §6's cost is measured and real — the numbers are at the end of that
section, along with the remedy that was tried, measured useless, and removed.
The slow tail is recorded in [later](../later.md); the client timeout it can
now reach answers with a sentence instead of a raw errno, shipped in the same
pull request because that is what made the path reachable.

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

An occurrence qualifies when the character immediately before it is not
alphanumeric, or when it sits at the very start of the body. Nothing is asserted
about where the occurrence *ends*.

**A message qualifies when *any* of its occurrences does.** The quantifier is
part of the rule rather than a detail of implementing it: a body can contain the
needle several times, and finding the first occurrence and testing that one is
both the obvious implementation and wrong.

That asymmetry is the whole design and is worth being explicit about, because
the symmetric rule is the one that first suggests itself. Requiring both ends to
sit on a boundary means whole-word matching, and **`start` must still match
`starting`** — typing a prefix and letting the tail fall where it may is what a
search box is for, and a rule that breaks it has traded one wrong answer for
another. Requiring only the start keeps that working while removing every hit of
the kind §1 shows, because an interior match is precisely one that does not
begin at a word start.

So the four cases a test should pin, in one line each. `start` matches
`starting`; `art` matches neither `started` nor `apartment`; `art` does match
`art deco`; and `art` matches the body

```
we started at six, is that art deco
```

whose first occurrence is interior to `started` and whose second is a real hit.
That fourth case is the one that separates the rule from a plausible
implementation of it — the first three all pass under a first-occurrence-only
check — and it is not a contrived body, being §1's noise example and §1's real
example in the same message, which is what a conversation about one subject
actually looks like.

Both existing scans already have the right shape to carry this — each is an
`any` over candidate positions, so the boundary test belongs inside that `any`
rather than in a wrapper around one found position.

**It has to go in both of them, and the ASCII one is the one that matters.**
`contains_ignoring_case` returns early for an ASCII needle, into
`contains_ignoring_ascii_case`; only a needle that leaves ASCII reaches
`run_contains`. Every needle in this document — `art`, `start`, `starting`,
`apartment`, `ing` — is ASCII and takes the early return, so a rule implemented
only in `run_contains` would be inert for all of them, and inert in a way none
of the four cases above would catch, since those needles are ASCII too. Measured
rather than read: with a marker in `run_contains`, the existing suite reaches it
for `café`, `zürich`, and `É` and for no ASCII needle at all.

§3 says how to keep that from being two places the rule can drift apart.

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

There is a second argument for the same placement, and it is the simpler one:
the decoded body is a `String`, so the character before an occurrence is a
character. On the raw blob the preceding bytes are not guaranteed to sit on a
character boundary at all, so `char::is_alphanumeric` from §4 has nothing
well-defined to be applied to.

So the prefilter keeps the substring test exactly as it is, and the boundary
rule goes in the decoded filter alone — the single `messages.retain` in
`fetch_messages`. The comment above it currently says the two predicates are
deliberately identical so they cannot disagree; it becomes the superset
statement instead, which is what `register_body_match` already documents.

**This is a new function, not a wrapper.** `contains_ignoring_case`,
`run_contains`, and `contains_ignoring_ascii_case` all return `bool`, and
nothing in the tree yields an occurrence offset — so there is no match position
for a caller to look before, and the rule cannot be applied by wrapping the
existing call. The decoded side needs a boundary-aware variant of the predicate
itself, after which the two call sites call different functions rather than the
same one twice.

**It takes `&str`, and that is the whole point of the signature.** The
paragraph above is a precondition, not a nicety: the rule needs the character
before an occurrence, which exists only where the haystack is known to be UTF-8.
Taking `&str` makes that a type rather than a comment the next reader has to
find. It also settles §2's split cleanly — inside a `&str` the ASCII fast path
goes back to being a pure optimisation over the same rule, rather than one of two
branches the boundary logic has to be kept in step across. The byte-offset
detail follows from it too: `contains_ignoring_ascii_case` scans
`haystack.windows(..)` and so yields a byte offset, where the preceding
character is `body[..at].chars().next_back()` — well defined on a `&str` and
meaningless on the blob.

Design that signature once, here: §7 asks that a future chat-name version share
one predicate rather than grow a second definition of what a word start is, and
that is a constraint on this function's shape.

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

## 5 Should there be a way back to substring matching? (DECIDED: no flag)

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

**Decided 2026-08-09: the default, with no flag**, on the recommendation above.
`--substring` waits for the first observed search that wants an interior match,
and arrives with that case attached.

## 6 It makes the widening loop work harder

`fetch_messages` over-fetches and then trims, because the SQL `LIMIT` bounds raw
prefilter matches while the decoded filter drops some of them. When it drops too
many it re-asks for four times as many rows and scans again.

This plan increases the drop rate by construction — every interior match is now
a prefilter hit that the decoded filter rejects — so needles that are common
inside words and rare at word starts are the new worst case. `ing` is the
obvious one: overwhelmingly a suffix, so nearly every candidate row would be
fetched, decoded, and thrown away.

The first ask is already generous, which matters for reading any measurement of
this. `asking` starts at `limit * 4 + 64` whenever there is a query, not at
`limit`, and then multiplies by four per round — so `-n 100` walks **464 →
1,856 → 7,424**, with a full body scan each pass, before `exhausted` stops it.

Worth measuring rather than guessing — `query-performance.md` exists because a
subquery that looked free was not. If it bites, the fix is not to weaken the
rule but to widen that same first ask for short needles, since needle length is
known before the query runs and short needles are exactly the ones that pay.
Note that it is the *same* expression, whose comment already argues for
generosity on the first ask for this reason — a second widening stacked on top
of one that went unnoticed is the easy mistake here.

**Measured when the body half shipped, on 763k messages: it bites, and the
remedy above does not help.** `e` went 1.7s to 2.8s and `start` sits at 3.3s —
needles that begin words fill the limit fast and barely pay. `art` went 9.1s to
12–23s depending on cache. `ing`, the predicted worst case, went from 3.4s of
pure noise to 15–20s of correct answers warm — and cold it walks past the
client's 30-second socket timeout, which surfaces as exit 1 and a raw `os
error 35` with no results. That failure was unreachable before this rule, since
nothing could hold the daemon past 30 seconds.

The first-ask widening was implemented and measured: no effect outside noise,
so it was removed rather than shipped as insurance. The rounds grow
geometrically, so everything before the last is cheap and the first ask is not
where the time goes — the cost is scan *depth*. A needle that rarely begins a
word makes the loop decode most of the table's substring candidates to reject
them, and no ask schedule changes how deep that walk has to go. What would:
streaming the scan backwards (`query-performance.md §9`), a scan cap with an
honest "stopped early" answer, or the index this repo has twice declined to
build. Shipped as-is, on the judgment that these searches previously returned
garbage instantly and now return the right thing slowly; the slow tail is
recorded in [later](../later.md). The timeout itself answers with words now —
which command, which deadline, that the daemon is working rather than crashed —
because a path a branch makes reachable ships with its error message, the
standard the snapshot fallback's raw `EPERM` set.

## 7 Scope: message bodies only

Chat and contact name matching is a separate substring test — `answers_to` on
`Contact`, and the `displayName`/`chatIdentifier`/`handles` clause in
`fetch_chats`. Those already have exact-match precedence in front of them,
which is what settled the case that prompted the nickname work, and the noise
this plan removes had not been observed there.

**It has been now, so this scope is wrong. (CORRECTED)** This section said to
leave name matching alone until the noise showed up. Reading a conversation by
a three-letter first name turned up two candidates that were entirely different
people, matched inside their surnames the same way `art` matches `apartment` —
the interior case, in the place a user is most certain they typed a whole word.
`naming-a-conversation.md §2` records it.

So name matching takes the rule as well, and the two share one predicate rather
than growing a second definition of what a word start is. That is a further
argument for §3's `&str` signature: a contact name is already one, so the same
function serves both callers unchanged.

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
