# Plan: Put a reaction on the message it reacted to

**Status:** Designed, not started. Committed to in [next](../next.md). The type
numbering in §2 is measured against the real database; the two open questions in
§9 are not, and the first of them decides how §4 gets built.

**Goal:** Render tapbacks as `[😂🙏♥♥]` after the message they react to, instead
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
messages, of which 622 are tapbacks. Types paired with the verb Messages
synthesizes into the body, which is what identifies each one.

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
| 1000 | no quoted target | 4 | unidentified, see §9 |

## 3 Three things that measurement settles

**The verb is localized, so the body cannot be parsed.** A single database holds
`Liked` and `Le gusta` for the same type 2001, `Loved` and `Le encantó` for 2000,
`Emphasized` and `Le sorprende` for 2004. The synthesized text follows the
sending device's language, not the reader's. Any design that reads the reaction
out of the body works until someone in the thread switches locale. **The type
number is the only reliable key**, which is what makes §4 a table in code rather
than a parser.

**The classic six dominate, so the shorthand is the main path and not a
fallback.** Of 619 reactions added, 575 — 93% — are one of the classic six, which
carry no emoji anywhere. Only 44 are an arbitrary emoji. The bracket this feature
prints will usually read `[+1+1♥]`, and `[😂🙏]` is the uncommon case. Worth
saying plainly, because the request was phrased around emoji and the output
mostly will not be.

**Removals are real, rare, and will show wrong data if ignored.** Three of 622.
Messages does not delete the reaction row; it inserts a second row in the 3000
range. Counting only the 2000 range therefore displays reactions that were taken
back. Rare enough to be invisible in testing and permanent once wrong.

## 4 The symbol table

The classic six get shorthand rather than emoji **(DECIDED — requested)**:

| Type | Reaction | Symbol |
| --- | --- | --- |
| 2000 | loved | `♥` |
| 2001 | liked | `+1` |
| 2002 | disliked | `-1` |
| 2003 | laughed | `LOL` |
| 2004 | emphasized | `!!` |
| 2005 | questioned | `?` |
| 2006 | emoji | the emoji itself |

*Rejected:* rendering the six as their Messages glyphs, `❤️👍👎😂‼️❓`. It would
look like the app and would make every reaction one column wide, but it was not
what was asked for, and an emoji-only bracket loses the distinction below.

Two wrinkles to accept knowingly. `♥` (U+2665) is a symbol where the other five
are ASCII, so the row is not uniform — `<3` would be, and reads worse. And `♥`
for *loved* will sit in the same bracket as a literal `❤️` from a type 2006
reaction, two glyphs that look alike and mean different things: one is the
built-in Love tapback, the other is someone choosing the heart emoji. That is a
real distinction faithfully rendered, not a collision to design away.

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

The protocol version bumps. **`search-context.md` also bumps it** — whichever
lands second takes the next number, and neither should assume 7.

## 8 Slices

1. **The attachment and the bracket.** `tapbacks_for`, the type table, removals
   cancelled, default rendering, `--tapbacks` suppressing it, JSON field,
   protocol bump. Everything in §4 through §7 except names.
2. **Names behind a flag.** Small, and separable because the sender is already
   carried on each entry by slice 1 — only the rendering is missing.

## 9 Open, and the first one blocks §4

**How is the emoji stored for type 2006?** Unresolved, and it decides whether
this feature can render its headline case. `associated_message_emoji` is the
column to look for. If it exists and is populated, use it and stop reading.

If it does not, the emoji is only in the body, wrapped in a localized sentence —
three shapes were observed for those 44 rows: `Reacted <emoji> to “…”`, the
Spanish `Se ha reaccionado con <emoji> a “…”`, and one form carrying no verb at
all, just the emoji fenced by zero-width spaces before ` to “…”`. Parsing the
verb is out for the reason §3 gives, but extracting *the emoji-class characters
before the opening curly quote* is locale-independent and matched all three
shapes in the sample. That is the fallback, and it needs the zero-width
characters stripped.

**Is `associated_message_guid` a bare guid?** Assume not until checked. The
plausible stored form is part-prefixed — `p:0/<guid>` — and a join written as
`= message.guid` would then match nothing while looking correct, silently
shipping a feature that renders no brackets on a machine whose data is fine.
`threading.md §2` compared the two columns and found them equal 71 times, which
says they are comparable but not that they are identical in form.

**What is type 1000?** Four rows, no quoted target, excluded by the `!= 0` test
today and so already invisible. Most likely a sticker placed on a message rather
than a reaction. Out of scope; identify it before assuming the 2000/3000 ranges
are the whole story.
