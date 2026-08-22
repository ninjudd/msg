---
status: next
---

# Plan: Read only the user-visible Contacts universe

**Status:** Written up, not started. The findings were measured 2026-08-09
and two options are recorded below, neither chosen — this document puts the
thinking down, and more investigation comes before either option is picked
up. The decision belongs to that later session, not to this write-up.

**Goal:** Stop `msg` answering with contacts macOS itself does not show.
Every database under `~/Library/Application Support/AddressBook/Sources/` is
read today as if it were the user's; two of the four on the machine measured
are not.

## 1 The complaint

Two real queries, minutes apart, on the day `contacts resolve` was built
(#54, since merged). The first —
one contact answered as "4 people match", her cards split across accounts —
was a modeling bug, fixed in #54 by unifying records filed under one
name (contact-resolution.md §5). The second survived that fix: a resolve
listed a card under a prank name, sharing a real person's email address,
from a set of contacts that belong to a family member and appear nowhere in
Contacts.app. The user's diagnosis, confirmed by everything below: the app
is behaving correctly, and `msg` is over-enumerating databases.

## 2 What was measured

All on one machine, 2026-08-09, through probes running in the daemon (the
established pattern: patch a log line in, build, install, read
`msgd.log`, revert). Four AddressBook databases exist: three under
`Sources/` — call them `SOURCE-A` (605 records), `SOURCE-B` (241), and
`SOURCE-C` (2) — plus the legacy top-level one.

- **`CNContactStore.containers(matching: nil)` names the user-visible
  universe**, and it lists exactly two: `SOURCE-A:ABAccount` and `_local`. The
  identifier is the source directory's UUID plus an `:ABAccount` suffix;
  `_local` is the legacy top-level database. `SOURCE-B` and `SOURCE-C` are not
  containers at all.
- **`SOURCE-B` is live but auxiliary.** Its account exists in the Accounts
  store and its database syncs actively — but the account is owned by
  `ZOWNINGBUNDLEID = com.apple.AddressBookSourceSync` with a *CardDAV*
  parent account, where the user's real source is owned by `mbuseragent`
  with an Apple ID parent. It is some subsystem's derived database —
  plausibly family or child account infrastructure — not a user container.
- **`SOURCE-C` is dead.** Its account identifier appears nowhere in the
  Accounts store, and its database was last touched months ago. An orphaned
  directory outliving its account.
- **Nothing simpler discriminates.** `ZLINKID` and the unification-override
  table: empty. The enabled/provisioned dataclass tables in the Accounts
  store: empty. Contacts' containerized preferences: only a sidebar
  selection and the default-source id. Account type: both sources read
  `com.apple.account.CardDAV`. The visibility and active flags: `1` across
  the board.

So the discovery, worth stating on its own: **the raw AddressBook directory
contains more than the user-visible Contacts universe**, and the platform's
definition of "yours" is only published through `CNContactStore` — which is
exactly the API `msg` deliberately does not call.

## 3 The constraint that shapes both options

Calling `CNContactStore` costs a Contacts permission prompt, from whichever
process is TCC-responsible. Run from a terminal, the prompt names *the
terminal* and the grant sprawls exactly the way this project exists to
prevent — measured live, and vetoed. A prompt is acceptable here only as
deliberate architecture, decided in a plan, never as a feature's side
effect. That rules out any quiet "just ask CN for the container list"
helper, and splits what remains into the two options below.

## 4 Option A: filter on the measured disk signatures, plus knobs

No prompt and no framework, but not quite "files the daemon already
reads": the Accounts store is a new protected input, read for account
identifiers, types, and owning bundles and never the credential tables —
a real scope expansion, named here because the daemon's privileged-data
footprint is part of choosing between these options. Auto-exclude a
`Sources/` database when the Accounts store says its account is gone (the
`SOURCE-C` case) or names `com.apple.AddressBookSourceSync` as the owning
bundle (the `SOURCE-B` signature); read everything else as today. Add
`contacts_exclude` and `contacts_include` keys to the config as the
override in both directions, honored by daemon and direct paths alike, and
`msg contacts sources` listing each source's UUID, record count, owning
bundle, and verdict, so there is something to point the knobs at. If the
Accounts store cannot be read, filter nothing — wrongly hiding a real
contact is the worse failure for an identity tool, and the knobs still
work.

The honest caveat: the signatures are one machine's measurements. Whether
`AddressBookSourceSync` ever owns a source a user *does* see — a
Google-contacts mirror, say — is unmeasured, which is exactly what the
include knob and the listing are for. The scope expansion above also gets a
section in daemon-and-permissions.md when this ships, since that document
is where the daemon's reads are enumerated.

## 5 Option B: contacts through the platform's API, behind its own permission

The deeper fix, floated by the user the same evening: perhaps reading
contacts *should* cost the Contacts permission. Today `msgd` reads
contacts under Full Disk Access, a blunt grant that never says the word
"contacts"; a `CNContactStore`-based reader behind the real permission
would name what it does and make the OS enforce what is currently only
architecture — *if* the prompt presents from launchd-agent context at all,
which is unmeasured and is the spike this option waits on. If it does, the
grant lands on `msgd` once and is revocable with
`tccutil reset AddressBook com.ninjudd.msgd`; if it does not, Option B
needs a different shape entirely.

What makes this more than permission theater: **the permission is real only
if the file-reading stops.** CN becomes the contacts source; FDA shrinks in
meaning to the Messages database. And CN gives back, by definition, most of
what this repo hand-derives and debugs: the container universe (this
plan's whole problem — gone), unified cards (the "4 people" bug — gone),
labels, and immunity to the next abcddb schema drift.

The costs, plainly: a Swift surface, either a bundle helper (user-writable,
so its answers are only as trustworthy as the bundle — acceptable for a
read path, but the swapped-helper-runs-with-msgd's-attribution channel
must be written down) or a linked framework, for which the
confirmed-and-delayed-send plan (#53, since merged) is already opening
the door and building the machinery; the unmeasured question of whether
the Contacts prompt presents from launchd-agent context, which is the same
spike #53's plan must run for `LAContext`; and the daemonless CLI, which
cannot use CN (terminal attribution) and either keeps raw file reads under
terminal FDA as a documented fallback or loses names.

## 6 How the options relate

A is shippable this week and B supersedes only its middle: the auto-exclude
signatures retire if CN ever names the containers, while the knobs and the
`sources` listing stay useful under any future. B waits, at minimum, on
the delayed-send spike answering the launchd-UI question both plans share.
Doing A now and keeping B on the list is the sequencing the discussion
ended on, recorded as a leaning rather than a choice: more investigation
comes before either option is started, and the decision lands then.

## 7 Interim

Until either option ships: #54's unification fix already answers
split-card contacts correctly, an exact full name resolves past the
auxiliary pollution, and the auxiliary source's presence is at least now
understood rather than mysterious.
