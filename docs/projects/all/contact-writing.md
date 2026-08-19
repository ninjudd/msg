---
status: Shipped
---

# Plan: Add and update contacts from the command line

**Status:** Shipped. Written and implemented in one pull request, after the
read half (contact-resolution.md, since merged) made `msg` the tool that
answers "who is somebody" and immediately raised the question it could not
answer: how does somebody new get *into* Contacts without leaving the
terminal? §8 records the first live run.

## 1 Why

`msg contacts resolve` turns a name into addresses, and nothing in `msg`
goes the other way. The errand that motivated this — a list of people with
phones, emails, titles, and notes, arriving in a message, wanted in
Contacts — ends today in the Contacts UI, one field at a time, or in an
osascript one-liner typed from a terminal, which costs an Automation grant
to *the terminal* and therefore to everything the terminal runs.

So: `msg contacts add` and `msg contacts update`. Writing goes to the same
Contacts the resolver reads, and the permission lands on the daemon, where
this project puts every grant it holds.

## 2 Writes run in the daemon (DECIDED)

The CLI holds no permission of its own and stays that way. A write from the
CLI would put the permission prompt on the terminal — measured and vetoed in
contact-sources.md §3, since a grant to the terminal is a grant to every
process it ever runs. The daemon is a signed bundle whose grants are keyed
to `com.ninjudd.msgd`, revocable one switch at a time, which is the entire
argument of daemon-and-permissions.md applied unchanged.

Without a daemon listening, both commands refuse with the same shape `send`
uses: install the daemon, which holds the permission.

## 3 Apple Events to Contacts.app, not CNContactStore (DECIDED)

Two mechanisms can write a contact. The Contacts *framework*
(`CNContactStore`) is the platform API contact-sources.md §5 sketches as
Option B for reads; Apple *Events* drive Contacts.app the way
`daemon/send.rs` already drives Messages. Writes take the second, for one
decisive reason and two supporting ones:

- **The note field is closed to the framework and open to the app.**
  `CNContactNoteKey` requires `com.apple.developer.contacts.notes`, a
  restricted entitlement granted by Apple per-application. A self-signed
  bundle cannot carry it at all — AMFI refuses to launch a binary claiming a
  restricted entitlement without an Apple-issued provisioning profile — so
  under the signing this project uses (signing-identity.md), a
  `CNContactStore` writer could never write a note, and a note is exactly
  the field that makes "put this person in Contacts with everything I know
  about them" whole. Contacts.app is itself entitled, and its scripting
  interface writes notes freely.
- **The permission path is proven, not spiked.** Whether the Contacts
  permission prompt presents from launchd-agent context is the unmeasured
  question Option B waits on. Apple Events from this daemon are measured:
  `msgd` holds an Automation grant for Messages today, obtained through
  exactly this flow. Writing contacts adds a second Automation row —
  msgd → Contacts, prompted on first write — under machinery §7 and §13 of
  daemon-and-permissions.md already document.
- **Zero build machinery.** No Swift compilation unit, no linker work, no
  new dependency; rust-rewrite.md §8's ledger stays at four. The Swift
  surface confirmed-and-delayed-send.md §3 plans is still coming for the
  reasons that plan owns; a write path that does not need it should not be
  the thing that forces it in.

What this does *not* decide: contact-sources.md's Option B, which is about
reads. The wire commands below name what they do, not how — a future
CN-backed reader (or writer, should the entitlement situation change) slots
in behind the same protocol.

Rejected: linking `CNContactStore` anyway and shipping without notes — it
trades the feature's point for architectural tidiness, and runs an
unmeasured spike on the critical path of an errand. Rejected: one big
AppleScript with the logic inside it — comparison and dedup live in Rust
where tests reach them (§6), and the scripts stay single-purpose the way
`SEND_TEXT` is.

## 4 The CLI surface

```
msg contacts add "Dana Reyes" --phone 3105551234 --email dana@example.com \
    --title "Principal Engineer" --org "Example Corp" --note "referred by Sam"
msg contacts update dana --title "Staff Engineer" --phone 3105556789
```

`add` takes the person's full name — first word is the first name, the rest
the last name — and refuses when someone already answers to exactly that
name, naming them, since the likely intent is `update`; `--duplicate`
overrides for the father-and-son case the resolver already models. `update`
resolves its term exactly as `contacts resolve` does — name, nickname, or
address; a fragment that matches several people is refused naming the
candidates, exit 3 — then applies the flags. Both print what changed, one
field per line, and take `--json`.

`--phone` and `--email` repeat. On update they append, except that a value
whose `handle_key` the person already carries is reported as already there
and skipped — so re-running a command is safe, and a number retyped in a
different shape does not become a second phone. `--title`, `--org`, and
`--note` set their field outright, replacing what was there; replacing is
idempotent, and read-modify-append on a note would need the note read back
through a parse there is no robust format for.

Exit statuses keep the resolve contract: 0 wrote, 1 ordinary failure,
2 the grant is missing, 3 the term or name was ambiguous.

## 5 The wire

`person-add` and `person-update`, protocol 18 → 19. Requests carry the
fields; no request carries a filesystem path, so §6 of
daemon-and-permissions.md is untouched. Both answer a `PersonWriteReply`:
the Contacts record id, the name as filed, `created`, and `changed` /
`unchanged` — field-per-entry lists the CLI prints as lines and `--json`
hands over structured.

The stale-daemon failure is the loud kind: an old daemon fails the version
gate, and behind it the `COMMANDS` check names the unknown command. Nothing
here changes the shape of an existing reply.

## 6 The seam that keeps tests out of real Contacts (DECIDED)

The daemon-side logic — the duplicate guard, resolve-then-apply, dedup by
`handle_key`, the changed/unchanged accounting — is plain Rust, and the
osascript boundary behind it is a trait (`ContactStore`): find ids by exact
name, create, read a field's values, set a field, append a value. The
production implementation is six small AppleScripts run the way `send.rs`
runs its two, arguments passed to `on run` rather than interpolated. The
test harness injects a fake store through `DaemonOptions`, the same door the
fixture AddressBook uses, so `cargo test` writes no one's contacts and
still pins every decision above the boundary.

The scripts themselves are exercised live, not in tests — the same line
`send.rs` draws, and the same reason §8 records measurements instead of
assertions.

## 7 What the update writes to, exactly

`update` resolves a term to one person through the daemon's index, then
addresses Contacts.app by that person's filed name. Records filed under one
name are one person to the resolver (contact-resolution.md §5) because that
is the unification Contacts itself displays, so when the app answers the
name with several cards, they are that person's cards across accounts and
the write goes to the first — the edit surfaces on the unified card either
way. Two strangers sharing an exact full name collapse in the resolver
before this code ever runs; that is a modeling limit this plan inherits
rather than adds. New contacts land in the account Contacts.app itself
files new people into.

## 8 Measurements

From the first live run, 2026-08-19, on the machine
daemon-and-permissions.md measured:

- **The Automation prompt presents from launchd-agent context for a second
  target app.** The first write raised "msgd wants access to control
  Contacts", it was approved at the machine, and the write completed —
  §13's machinery confirmed for a target other than Messages. This is also
  half an answer to the launchd-UI question contact-sources.md §5 and
  confirmed-and-delayed-send.md §3 share: a *TCC* prompt presents from this
  context; whether an alert or `LAContext` sheet of the daemon's own
  presents is still that spike's to run.
- **Driving Contacts.app launches it** when it is not already running,
  without bringing a window forward.
- **Latency.** An idempotent update — resolve the term, find the card, read
  its phones, write nothing — round-trips in 0.75s. A five-field `add`,
  which is six scripts each ending in a `save`, runs in a few seconds.
- **A new card resolves within seconds of the write.** The daemon drops its
  cached index after any write, and contactsd had synced the new record
  into the AddressBook databases by the first resolve after the add.
- **An appended value is stored under Apple's `other` label**, not
  unlabeled: it comes back from `resolve` as `label: "other"`, and the
  Contacts UI shows `other` beside it.
- **Clearing works as replacing.** `--note ""` on five cards emptied the
  note on each; nothing distinguishes a cleared field from one never set.
