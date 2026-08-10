# Plan: Contact resolution as a public primitive

**Status:** Designed, not started. The design was settled in discussion on
2026-08-09; nothing is implemented, and §9's measurement has not been run.

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
gog gmail search "from:$(msg contacts resolve dana --email)"
```

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

The domains differ even though the rules do not: `resolve` answers from all
of Contacts, while `chat` goes on to require a conversation. Someone never
messaged resolves fine and then has no chats, which is two different
questions getting two different answers, not a disagreement.

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
| 2 | the databases could not be opened |
| 3 | more than one person matched |

The stdout discipline is what makes `$( … )` safe: an ambiguous name must
produce an empty-and-failed substitution, never a wrong address spliced into
somebody else's command line. Ambiguity gets its own status because it is
the branch scripts most need to take — "ask a human" rather than "give up".
Exit 2 keeps its documented meaning unchanged. README and the Agent Skill
document the contract in the shipping PR.

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

## 10 Through the daemon

`resolve` rides `source.rs` like every other read: answered by the daemon
when one is listening — Contacts sits behind the same Full Disk Access the
daemon exists to hold — and by reading the databases directly when not. One
new request in the protocol, read-only, taking a term and returning the
person object; it accepts no path from anyone, so it changes nothing about
what the socket can be made to do.

## 11 Out of scope

Query builders — `from:(a OR b)` for Gmail, or any per-tool formatting.
The boundary, stated the way the daemon plan states its own: `msg` answers
who somebody is; what an identifier is for belongs to the caller. A caller
wanting a compound query builds it from `--emails` in two lines of shell.
If demand for helpers materializes, they arrive as a separate plan, priced
as conveniences rather than as part of the identity contract.

## 12 Shape of the work

One PR is the likely right size: the person assembly in `contacts.rs` with
existing consumers migrated, the subcommand with its four output modes, the
protocol request, the tests — the one-resolver fixture of §3, the exit
statuses, singular-flag failures, label extraction against a fixture
database — and the README, skill, and list edits that ride every shipping
PR here. If it splits at all, the seam is the §2 assembly refactor landing
first with `--with` and chat resolution migrated, but that half changes no
behavior, and a PR that changes no behavior is exactly the shape the sizing
convention argues against shipping alone.
