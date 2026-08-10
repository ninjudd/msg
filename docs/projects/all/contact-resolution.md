# Plan: Contact resolution as a public primitive

**Status:** Shipped, 2026-08-09, in one PR as §12 sized it. §9's measurement
ran on a real database first and is recorded there; §5 gained the
record-per-person note and §9 the separator rule during implementation, each
marked in place.

**Goal:** Expose the name → person stage of resolution as `msg contacts
resolve` — stable JSON, shell-friendly output, explicit ambiguity — so other
tools can ask who a name means without knowing anything about iMessage.

## 1 Why

Resolution inside `msg` is already a two-stage pipeline: a name becomes a
Contacts record, and the record becomes conversations. The second stage is
iMessage; the first is not, and it is the part every other tool lacks — the
part that knows Dana answers to `dana`, that Bob is filed as Robert Chen,
that `(310) 555-1234` and `+13105551234` are one number, and that two people
sharing a name is a question to report rather than answer.

```text
                     ┌→ iMessage (msg chat, msg send)
human name → contact identity
                     ├→ Gmail (gog)
                     └→ anything else that takes an address
```

`msg` stays responsible for macOS Contacts and identity; callers stay
responsible for what an identifier is for. They compose through ordinary CLI
output:

```sh
email=$(msg contacts resolve dana --email) &&
gog gmail search "from:$email"
```

The guard is part of the composition. §7 keeps stdout empty on failure
precisely so the `&&` has something honest to test — pasted inline instead,
a failed substitution hands `gog` an empty `from:` and the search runs
anyway, broader than anything the caller meant.

## 2 What exists, and the one real gap

`ContactIndex` is handle-shaped, not person-shaped: a map of normalized
handle to `Contact`, where each entry is one (address, person) pair and the
person emerges from entries sharing a record id. Matching, tie-breaks,
nickname displacement, and number normalization all exist and are tested.
What does not exist is the person object itself — every consumer that needs
one (`--with`, chat resolution) assembles it ad hoc.

`resolve` is the first feature whose *output* is the person, so the core of
the work is to assemble that object once, in `contacts.rs`, and migrate the
existing consumers onto it. The command is then mostly rendering.

## 3 The one-resolver guarantee (DECIDED)

`msg contacts resolve dana` names the same person `msg chat dana` would open:
same word-start matching, same exact-name tie-break, same ambiguity behavior.
One resolver, several consumers, pinned by a test that resolves the same
fixture both ways.

Rejected: letting the new command grow its own matching rules. Two resolvers
drift, and the first time they disagree, `resolve`'s answer is a lie about
what `send` would do — the exact failure the dry-run address exists to
prevent.

The rules are shared; the domain is each command's own. `chat` resolves
among people present in the Messages handle table — that is what
`people_matching` reads — while `resolve` answers from Contacts. The visible
consequence, decided here rather than discovered later: a name two Contacts
records answer to, only one of them ever messaged, opens uniquely in `msg
chat` and reports ambiguity from `resolve`. That is the two questions
differing, not the rules — "who that I message is Dana" has one answer on
that machine, "who is Dana" has two, and a caller who means the messaged one
says so with an address. Rejected: letting Messages presence break the
`resolve` tie, which is §8's rejection again — an iMessage signal quietly
deciding a generic identity answer. The §3 fixture pins this case from both
sides.

## 4 Command grammar (DECIDED)

`msg contacts resolve <term>`, a subcommand beside the existing positional
lookup. `resolve` becomes a reserved word where a name could stand today —
`msg contacts resolve` currently reads it as a search term — which is an
accepted break: the collision is somebody actually named Resolve.

Rejected: a top-level `msg resolve`, which spends a top-level command on
something that is conceptually part of `contacts`; and a `--resolve` flag,
which hides a mode switch — different output, different failure contract —
inside an option.

The existing `msg contacts <terms>` behavior is untouched. It is an
annotator: it happily lists several people, because identifying whoever
matched is its whole job. `resolve` is a picker, and the two coexist the way
`chats` and `chat` do.

Output modes, spelled out. Bare `resolve <term>` prints for a person: the
name line `msg contacts` prints today, then one address per line with its
label — human-first, like every read command here. `--json` emits the §5
object. `--emails` and `--phones` emit values one per line; `--email` and
`--phone` are §8's singular forms. Those five flags are mutually exclusive,
enforced by clap: a script wanting values has the line forms, one wanting
structure has the object, and `--json --email` has no meaning that is not
better spelled one of those two ways.

## 5 The schema (DECIDED)

Identifiers are objects from day one:

```json
{
  "id": "4A5C…:12",
  "name": "Bob",
  "filedAs": "Robert Chen",
  "emails": [
    { "value": "bob@example.com", "label": "home" }
  ],
  "phones": [
    { "value": "+13105551234", "label": "mobile" }
  ]
}
```

- **Objects, not strings**, because string → object is the one breaking
  change already visible from here, and object → more fields is free
  forever. After v1 the schema evolves additively only.
- **`name` and `filedAs`, not `name` and `nickname`.** The nickname is what
  you call someone, so it is what they are called here — same rule, same
  vocabulary as `msg contacts --json` today. A `resolve` that renders the
  same person differently from every transcript would be the inconsistency
  the nickname design exists to prevent. `filedAs` is absent when nothing
  was displaced.
- **`id` is opaque and documented as not durable.** The internal id is
  source UUID plus Core Data primary key — unique for one run, not promised
  across resyncs. If the AddressBook schema turns out to carry a durable
  record UID, tightening the promise later is additive; promising now and
  retracting is not. Verify against the database before claiming more.
- **The domain is address-bearing contacts, stated plainly.** The loader
  builds the index from the phone and email tables, so a record with
  neither — a notes-only card, a company shell — never enters it, and
  `resolve` reports nobody matched for a name Contacts.app would show.
  Documented rather than fixed in v1: this command exists to hand back
  identifiers, and a hit carrying none leads a caller to the same place as
  a miss. If a real case ever proves otherwise, the change is a person-led
  load emitting the object with empty arrays and status 0 — additive, and
  waiting on that case per the rule against speculative fixes.
- **One record is one person** (added at implementation). The same human
  held in two accounts is two records, and where both answer a name,
  `resolve` reports the tie rather than merging them — which is what
  conversation resolution already does, so §3's guarantee requires it.
  Merging across sources by shared address is future work waiting on a real
  complaint, and it would have to move both resolvers together. One carve-out
  (added when review caught the map suppressing shared addresses): an
  *address* held by several records answers with the source-preferred record
  when every holder renders one name, since one human synced twice is not
  two people — and stays an ambiguity when the names differ, which is the
  parent-and-child-on-one-line case no rule may pick from.

## 6 Labels

The loader does not read labels today; the schema requires them, so v1 adds
label reading rather than shipping `"label": null` everywhere. The label
tables' names, shapes, and how unlabeled entries read are schema assumptions
to verify against a real AddressBook database first, per this repo's
standing rule. A value with no label omits the field.

## 7 Ambiguity and exit statuses (DECIDED)

On any failure, stdout stays empty. Candidates go to stderr, one per line
with an address alongside each, and the exit status says what happened:

| Status | Meaning |
| --- | --- |
| 0 | one person, output emitted |
| 1 | nobody matched |
| 2 | the databases could not be opened or read, in whole or in part |
| 3 | the answer is not unique — several people, or several values under a singular flag |

The stdout discipline is what makes `$( … )` safe: an ambiguous name must
produce an empty-and-failed substitution, never a wrong address spliced into
somebody else's command line. Ambiguity gets its own status because it is
the branch scripts most need to take — "ask a human" rather than "give up".
Exit 2 keeps its documented meaning unchanged. README and the Agent Skill
document the contract in the shipping PR.

Status 3 is ambiguity of either kind, deliberately one number: `--email` on
a person with two addresses fails exactly as two people named Dana do, with
the candidates on stderr saying which kind this was. Rejected: a separate
status per kind — both recoveries end at a human or at the plural form, and
a contract grows statuses more easily than it retires them.

A partial Contacts load is an error here, not a quieter success. The loader
skips an unreadable source, records the problem, and serves the rest —
right for rendering names in a transcript, where best-effort beats blank,
and wrong for an identity answer a script will act on: a "nobody matched"
that is missing a source is not a fact, and neither is an "exactly one"
whose duplicate might live in the database that failed to open. `resolve`
with any load problem recorded exits 2 and names the failed source on
stderr, even when a healthy source held a match. Rejected: carrying
rendering's best-effort silence over to resolution. Tests pin both halves —
the failed source forcing 2 over an otherwise-clean match, and a fully
healthy load resolving normally.

## 8 Singular `--email` and `--phone` (DECIDED)

Singular means exactly one after dedup. If a person has two email addresses,
`--email` fails with status 3 and both values on stderr; `--emails` is the
form that emits them all, one per line, as `--phones` does for numbers.

Rejected: a preference order over labels (home beats work?), which is policy
inside what has to stay a primitive — macOS Contacts no longer has a usable
"primary" concept to defer to. A later `--label work` filter would hand the
policy to the caller instead. Also rejected: defaulting to the address most
recently active in Messages. That signal is real and only `msg` has it, but
it is an iMessage signal — where Dana texts from says nothing about where
she reads Gmail — and using it silently smuggles iMessage back into the
layer this plan exists to keep generic. As an explicit flag someday, maybe.

## 9 Phones on the way out (DECIDED, pending one measurement)

Numbers are stored in whatever shape they were typed, and the matching key
is deliberately lossy — right for matching, wrong for emitting. True E.164
output would mean inventing a country code for a bare `3105551234`, a guess
that is silently wrong abroad.

So: dedupe by the existing normalized key, emit E.164 when the stored form
already carries a country code, and the stored shape otherwise. Before
implementation, measure what fraction of stored numbers carry one, on a real
database, as an aggregate. If nearly all do, the honest rule is also the
useful one and this section stands; if not, reopen it here rather than
patching the behavior quietly.

Measured at implementation (2026-08-09, through a probe running in the
daemon): 64% of 874 stored numbers carried a `+`-prefixed country code, 36%
did not, and `ZCOUNTRYCODE` — a column that looked like it might settle the
derivation — was empty on every row. Not "nearly all", so this section
reopened as promised, and the rule above was refined rather than replaced:
**separators are dropped from every number**, the `+` is kept when stored
and still never invented, and a value that is not digits after separator
stripping — an extension, a word — passes through as stored. What it
replaced: emitting the raw stored shape for CC-less numbers, which at 36%
would have made a third of all output `(310) 555-1234`-shaped and pushed
every consumer to write its own stripper. The no-invention half stands
unchanged.

## 10 Through the daemon

`resolve` rides `source.rs` like every other read: answered by the daemon
when one is listening — Contacts sits behind the same Full Disk Access the
daemon exists to hold — and by reading the databases directly when not. One
new request in the protocol, read-only, taking a term and returning the
person object; it accepts no path from anyone, so it changes nothing about
what the socket can be made to do.

The wire has to carry the failure contract, not only the object. Today just
`AccessDenied` and `SendDisabled` survive the socket typed; every other
error lands as `Other`, which the CLI maps to status 1 — so daemon-mode
ambiguity would exit 1 while the direct path exits 3, and §7 would hold or
break depending on whether a daemon happened to be listening. The request's
error path therefore carries ambiguity structurally — a code plus the
candidate list — and the client maps it back to the same status and the
same stderr shape as the direct path. One test drives `resolve` through a
real socket and asserts the two paths agree on every row of §7's table.

## 11 Out of scope

Query builders — `from:(a OR b)` for Gmail, or any per-tool formatting.
The boundary, stated the way the daemon plan states its own: `msg` answers
who somebody is; what an identifier is for belongs to the caller. A caller
wanting a compound query builds it from `--emails` in two lines of shell.
If demand for helpers materializes, they arrive as a separate plan, priced
as conveniences rather than as part of the identity contract.

## 12 Shape of the work

One PR is the likely right size: the person assembly in `contacts.rs` with
existing consumers migrated, the subcommand with the §4 output modes, the
protocol request, the tests — the one-resolver fixture of §3, the exit
statuses, singular-flag failures, label extraction against a fixture
database — and the README, skill, and list edits that ride every shipping
PR here. If it splits at all, the seam is the §2 assembly refactor landing
first with `--with` and chat resolution migrated, but that half changes no
behavior, and a PR that changes no behavior is exactly the shape the sizing
convention argues against shipping alone.
