# Plan: Put a reaction on the message it reacted to

**Status:** Designed, not started. Committed to in [next](../next.md). The type
numbering in §2 is measured against the real database, and the two questions §9
opened are now measured too (2026-08-09): the emoji lives in its own column,
and the guid is part-prefixed 96% of the time. §4's symbols reversed the same
day, from shorthand to the Messages emoji, by request.

**Goal:** Render tapbacks as `[😂🙏❤️❤️]` after the message they react to, instead
of as separate rows that either interleave into the transcript or are hidden
entirely.

## 1 What it looks like today

A tapback is an ordinary `message` row with `associated_message_type != 0`, and
`msg` does exactly two things with it: derives `is_tapback` from that column, and
drops the row unless `--tapbacks` is passed. Neither is what a reader wants.

Without the flag, reactions are invisible — a message everyone loved reads the
same as one nobody answered. With it, they interleave as their own lines,
`Liked "see you at 8"`, quoting a message three lines above and pushing the
conversation apart. The information is there in both modes and legible in
neither.

`message.associated_message_guid` — the pointer from a reaction to its target —
is never read. That column is the whole feature.

## 2 What the database actually holds

Measured through the daemon across the 25 most recent conversations: 8,165
messages, of which **626 carry a nonzero `associated_message_type`** — which is
the whole of what `is_tapback` means today — and **622 fall in the 2000 and 3000
ranges**. Every other figure in this plan counts the 622. The four type 1000
rows are in the table and outside that total, because nothing yet identifies
them (§9); they are tapbacks by the program's definition and not by any
definition this plan can act on.

Types paired with the verb Messages synthesizes into the body, which is what
identifies each one.

| Type | Verb observed | Count | Meaning |
| --- | --- | --- | --- |
| 2000 | `Loved`, `Le encantó`, `Le encanta` | 153 | loved |
| 2001 | `Liked`, `Le gusta`, `Le gustó` | 276 | liked |
| 2002 | `Disliked`, `No le gusta` | 2 | disliked |
| 2003 | `Laughed at` | 76 | laughed |
| 2004 | `Emphasized`, `Le sorprende` | 66 | emphasized |
| 2005 | `Questioned` | 2 | questioned |
| 2006 | `Reacted <emoji> to`, and two more shapes | 44 | arbitrary emoji |
| 3000 | `Removed a heart from` | 2 | loved, taken back |
| 3003 | `Removed a laugh from` | 1 | laughed, taken back |
| 1000 | no quoted target | 4 | unidentified, see §9 — *outside the 622* |

## 3 Three things that measurement settles

**The verb is localized, so the body cannot be parsed.** A single database holds
`Liked` and `Le gusta` for the same type 2001, `Loved` and `Le encantó` for 2000,
`Emphasized` and `Le sorprende` for 2004. The synthesized text follows the
sending device's language, not the reader's. Any design that reads the reaction
out of the body works until someone in the thread switches locale. **The type
number is the only reliable key**, which is what makes §4 a table in code rather
than a parser.

**The classic six dominate, so §4's table is the main path and not a
fallback.** Of 619 reactions added, 575 — 93% — are one of the classic six, which
carry no emoji anywhere. Only 44 are an arbitrary emoji. The bracket this
feature prints will usually come straight from the table, and an emoji read
off the row is the uncommon case.

**Removals are real, rare, and will show wrong data if ignored.** Three of 622.
Messages does not delete the reaction row; it inserts a second row in the 3000
range. Counting only the 2000 range therefore displays reactions that were taken
back. Rare enough to be invisible in testing and permanent once wrong.

## 4 The symbol table

The classic six get their Messages emoji **(DECIDED — requested 2026-08-09,
reversing the shorthand this section first chose)**:

| Type | Reaction | Symbol |
| --- | --- | --- |
| 2000 | loved | `❤️` |
| 2001 | liked | `👍` |
| 2002 | disliked | `👎` |
| 2003 | laughed | `😂` |
| 2004 | emphasized | `‼️` |
| 2005 | questioned | `❓` |
| 2006 | emoji | the emoji itself, from the column §9 measured |

*Rejected:* the shorthand this section originally decided on the same grounds —
requested — `♥` `+1` `-1` `LOL` `!!` `?`. What reversed it was seeing it
rendered: the non-emoji glyphs read badly, `♥` a hairline text-presentation
symbol next to everything else in a terminal that renders emoji at full width.

The reversal knowingly gives up the distinction the shorthand preserved: `❤️`
for the built-in Love tapback is now the same glyph as a literal `❤️` someone
chose as a type 2006 reaction. The two mean different things and render
identically. The shorthand kept them apart and was judged not worth its
rendering; if the distinction ever earns its way back, this is where it went.

## 5 Where it attaches

The same shape the reply work already uses, which is the argument for it: after
the limit and the body filter, `fetch_messages` collects the surviving rowids and
makes one side query — `attachments_for`, then `replies_for` — and hangs the
result on each `Message`. A `tapbacks_for` slots in beside them, keyed on the
guids of the messages being returned rather than their rowids.

Three consequences of putting it there. It only ever asks about messages actually
being returned, so a page of 50 costs one extra query. It inherits the clone-not-
take rule those two both carry a comment about: one message in two conversations
comes back as two rows with one rowid, and removing from the map gives the
reactions to whichever arrived first. And the `IN (...)` list needs the same
chunking the attachment lookup already has — `db.rs` carries a test named for
surviving more rowids than SQLite will bind.

## 6 Decisions

**Attached rendering is always on; `--tapbacks` keeps its current meaning.
(DECIDED)** Brackets appear by default, and are skipped entirely when a message
has no reactions — the `attachments`/`reply_to` habit, so ordinary output is
unchanged. `--tapbacks` continues to mean "show reaction rows as their own
messages", which stays useful for timestamps and for debugging, and it
*suppresses* the brackets, since printing both is the same information twice.
*Rejected:* repurposing `--tapbacks` to mean "show who reacted". It would break a
documented flag to save inventing one.

**Names are off by default, behind a flag. (DECIDED — requested)** Default
`[😂♥♥]`; with the flag, `[😂 dana, ♥ sam, ♥ kit]`. Naming the flag is left to
the slice.

**Duplicates are kept and ordered by reaction time. (DECIDED)** `[♥♥]` is two
people, and collapsing it to `[♥×2]` or `[♥]` throws away the count, which is the
main thing a bracket communicates at a glance. Chronological rather than grouped
by type, so the bracket reads as what happened.

**A removal cancels its add. (DECIDED)** Match a 3000-range row to the reaction
it retracts by sender, target guid, and type family, and drop both. §9 records
what still needs checking about the pairing.

**No reaction to a reaction.** Not a decision so much as a fact worth writing
down: a tapback's own guid can be a target in principle, and this renders one
level. If the data turns out to nest, the bracket goes on the tapback row that
`--tapbacks` prints and nowhere else.

## 7 JSON and the protocol

`Message` gains `tapbacks: Vec<Tapback>`, `skip_serializing_if = "Vec::is_empty"`
— the same treatment `attachments` gets, and for the same reason: most messages
have none and would otherwise carry `[]`.

Each entry holds the raw `associatedMessageType`, the rendered `symbol`, the
`date`, and the sender's `handle` and `contactName`. The type is published beside
the symbol deliberately: a consumer that wants Messages' own glyphs, or wants to
count Love separately from a heart emoji, should not have to re-derive it from a
string this program chose.

The protocol version bumps. **[search-context](search-context.md) also bumps
it** — whichever lands second takes the next number, and neither should assume
7.

The link points that way round on purpose. That plan is the base this one is
stacked on, so it merges first and its file is always already there; a link from
it to this one would resolve to nothing for the whole interval between the two
merges. A forward reference from a base to its child is the one direction
ordering cannot make safe.

## 8 Slices

1. **The attachment and the bracket.** `tapbacks_for`, the type table, removals
   cancelled, default rendering, `--tapbacks` suppressing it, JSON field,
   protocol bump. Everything in §4 through §7 except names.
2. **Names behind a flag.** Small, and separable because the sender is already
   carried on each entry by slice 1 — only the rendering is missing.

## 9 Open when written, measured 2026-08-09

Both questions were measured whole-database through the daemon — hard-coded
aggregate queries in a local spike, no message content leaving the process.

**How is the emoji stored for type 2006? (MEASURED: its own column.)**
`associated_message_emoji` exists and is populated for every emoji reaction
this database holds — 758 of 758 type 2006 rows, and the 7 type 3006 removals
besides. So the rule this section wrote in advance applies: use the column and
stop reading. The fallback that would otherwise have been needed — extracting
emoji-class characters before the opening curly quote, since the three observed
body shapes (`Reacted <emoji> to “…”`, `Se ha reaccionado con <emoji> a “…”`,
and a verbless form fenced in zero-width characters) share only that much — is
recorded here and deliberately not built.

**Is `associated_message_guid` a bare guid? (MEASURED: 96% part-prefixed.)**
32,092 rows carry the guid — the denominator this section said nothing had
measured. 30,800 of them are part-prefixed `p:N/<guid>`, and 30,766 of those
join to `message.guid` once the prefix is stripped. The other 1,292 are bare:
404 join directly, and 888 resolve to no surviving message — reactions whose
target is gone, which render nowhere and must not error. So the join §4's
builder writes strips the prefix *and* accepts the bare form; written
`= message.guid` it would have matched 404 rows of 32,092, the working-sometimes
failure this section predicted, delivered at scale. The earlier inference from
`threading.md §2` — 71 bare rows attested, prefix form unknown — pointed the
right way and undercounted both forms.

Scale, while the queries were open: the whole database holds roughly 31,900
tapbacks against the ~58,000 the sample scaling guessed — same order of
magnitude, the guess high by half. The removal ranges hold 76 rows, and the
guid also appears on types outside the ranges §2 sampled: 84 rows of type 1000,
24 of type 2007, and a handful of types 2 and 3.

**What is type 1000?** Whole-database, 84 rows, not the sample's four. Still
excluded by the `!= 0` test today and so already invisible. Most likely a
sticker placed on a message rather than a reaction. Out of scope; identify it —
and type 2007, which the measurement surfaced with 24 rows — before assuming
the 2000/3000 ranges are the whole story.
