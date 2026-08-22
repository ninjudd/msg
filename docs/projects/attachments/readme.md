---
status: done
---

**Outcome:** Shipped.

# Plan: Read attachments

**Status:** All three slices done. A message that is only a photo says so and
carries the id of the file behind it, `--json` has the same as structured data,
and `msg save <id> --to <dir>` writes the bytes out. §9 records what slice 3
spends of §3's permission argument, and §8 the measurement that changed how a
size is reported.

**Goal:** Stop a conversation with photos in it reading as holes in the text.

## 1 What it looks like today

Messages stores an attachment as a U+FFFC OBJECT REPLACEMENT CHARACTER in the
message body, and the file itself lives outside the database. `msg` prints the
body as it finds it, so the placeholder is all a reader gets: a message that is
nothing but a photo prints as one invisible character, and a message with text
around a photo prints with a gap in it.

That is the specific complaint. It is not that attachments cannot be downloaded
— it is that the tool currently shows you something *wrong*, a message that
looks empty when it is not.

## 2 What the schema actually says

Read out of the daemon's log against the real database, because the terminal
holds no Full Disk Access. DDL and aggregates only, no rows.

```sql
CREATE TABLE attachment (
  ROWID INTEGER PRIMARY KEY AUTOINCREMENT, guid TEXT UNIQUE NOT NULL,
  created_date INTEGER, start_date INTEGER, filename TEXT, uti TEXT,
  mime_type TEXT, transfer_state INTEGER, is_outgoing INTEGER,
  user_info BLOB, transfer_name TEXT, total_bytes INTEGER,
  is_sticker INTEGER, hide_attachment INTEGER, ...)

CREATE TABLE message_attachment_join (
  message_id INTEGER REFERENCES message(ROWID) ON DELETE CASCADE,
  attachment_id INTEGER REFERENCES attachment(ROWID) ON DELETE CASCADE,
  UNIQUE(message_id, attachment_id))
```

| Measured | Count |
| --- | --- |
| Attachments | 76,317 |
| Join rows | 75,916 |
| Messages with `cache_has_attachments = 1` | 55,977 |
| Messages with more than one attachment | 13,148 |
| `filename` NULL or empty | 1,301 |
| `hide_attachment = 1` | 17,832 |
| `mime_type` NULL | 17,624 |
| `is_sticker = 1` | 645 |
| `transfer_state = 0` | 69,430 |

Four things in there change the design, and three of them are not what I would
have guessed.

**There is an index for this already.** `message_idx_cache_has_attachments` on
`message(cache_has_attachments)`, and `message_attachment_join_idx_message_id`.
So "does this message have attachments" is a column read rather than a join, and
the join itself is indexed from the message side. Nothing here needs to cost
what the body scan cost.

**A quarter of them are hidden.** `hide_attachment = 1` on 17,832 rows, and
17,624 have a NULL `mime_type` — close enough to the same population to be worth
checking rather than assuming. Until that is understood, showing them is a
decision, not a default. See §7.

**The path is not always where you would look for it.** 74,514 filenames sit
under `~/Library/Messages/Attachments`, but 378 are elsewhere under `~/Library`
and 124 are under `/var/folders`. A prefix check written to keep the daemon
honest would silently refuse real attachments, so it must not be written that
way.

**Not every attachment is on disk.** 1,301 have no filename at all, and about 9%
have a non-zero `transfer_state`. "Not downloaded" is a state to render
honestly, not an error and not a broken path.

## 3 The permission question, and why it is not the same as sending

Sending already moves a file, and it moves it the safe way round: the client
reads it with the *caller's* permissions and hands the daemon bytes, so the
daemon never opens a path a caller named. Reading is the reverse and it does not
get to reuse that shape. The file is inside the grant, the caller has no grant,
and only the daemon can open it.

[daemon-and-permissions.md §6](../daemon-and-permissions/readme.md) already settled how:
**attachments are addressed by rowid, never by path.** The caller names a row in
`chat.db`; the daemon resolves the filename itself and never accepts one. That
keeps the daemon from becoming a general-purpose reader with Full Disk Access
behind it, which is the whole reason it exists.

Two consequences worth stating before any of it is built. The set of files
reachable this way is exactly the set named by an `attachment.filename` in the
user's own database — bounded, enumerable, and all Messages content. And §2's
finding above says the bound cannot be enforced with a path prefix, so it has to
be enforced by construction: the only way to name a file is a rowid, and the
lookup goes rowid to path in one direction only.

## 4 What to show, and what not to

`transfer_name` rather than `filename`. The first is the name the sender gave;
the second is an absolute path that discloses the layout of the user's home
directory to anything reading `--json` output. The path is what the daemon needs
internally and not what a reader needs.

Alongside it, whichever of `mime_type` and `uti` is present, and `total_bytes`
rendered in human units. A sticker and a not-yet-downloaded file each say so.

**And the rowid, first.** Added with slice 3, because it is the only way to name
this file: the path is deliberately not published, and `msg save` takes an id. A
reader looking at a transcript has nothing else to type, so printing it here is
what makes the command usable at all — without it there would have to be a second
command whose only job was to list ids.

## 5 The placeholder is positional

13,148 messages carry more than one attachment, so substitution has to be in
order rather than one-per-message: the *n*th U+FFFC in the body is the *n*th
attachment by join order. A message can also carry a placeholder with no
attachment row behind it, and an attachment with no placeholder in the body, so
neither side can be assumed to line up — leftover placeholders and leftover
attachments both need a defined rendering.

## 6 Slices

1. **Show what is there.** Replace each placeholder with a description built
   from the columns in §4. No file is opened; the daemon reads only `chat.db`,
   which it already reads. This is the whole of the complaint in §1 and it
   carries none of §3's weight.
2. **Say which messages have attachments in `--json`**, as structured data
   rather than as text baked into the body, so a consumer can act on it.
3. **Get the bytes out**, by rowid, through the daemon. This is where §3 applies
   in full, and it should not be started until the first two are in and the
   question in §7 is answered.

## 7 What `hide_attachment` turned out to be

Answered before building anything, because a quarter of the output depended on
it. Measured against the real database:

| | Hidden | Visible |
| --- | --- | --- |
| Body carries a U+FFFC | 483 | 57,884 |
| Of the population | 17,832 | 58,485 |

**A hidden attachment is almost never referenced by the message body.** 2.7% of
them have a placeholder, against 99% of visible ones. Whatever they are, the
conversation does not point at them.

The rest of the shape agrees. None are stickers. 16,931 of them have a filename
ending in `Attachment` — no extension at all — which is why `uti` is the
synthesized `dyn.age81…` macOS invents for a file it cannot type, and why
`mime_type` is NULL on 17,342 of them. Visible attachments look nothing like
this: `jpeg`, `heic`, `png`, real extensions throughout.

**So they are excluded from what gets rendered.** They are bookkeeping, and
showing them would put noise against a quarter of all attachments.

That leaves the 483 hidden rows that *are* referenced, and §5's rule is what
makes excluding them safe rather than lossy: placeholders and attachments are
matched positionally, and a leftover on either side has a defined rendering. A
message whose counts disagree degrades to a generic description of an
attachment, not to a wrong one and not to a crash.

## 8 `total_bytes` is not the size of the file

Measured while checking the first real saves: of six attachments taken from one
conversation, three had a `total_bytes` that disagreed with the bytes on disk —
one by more than a factor of two, and one where the *file was larger* than the
recorded size. That last one rules out the obvious explanation: a truncated
transfer cannot produce a file bigger than expected.

The transfer itself is exact. All six files matched, to the byte, the count the
daemon reported having read. So `total_bytes` is what Messages recorded about
the transfer, and it is not a reliable statement about the file it ended up
with.

Two consequences, both already in the code. The description in a message body
shows `total_bytes`, because that is a rough "how big is this" and being off is
harmless there. `msg save` reports the count it actually wrote, which is the
number that has to be right — and the two being different fields rather than one
is deliberate.

Anyone tempted to "fix" the mismatch by trusting `total_bytes` — by preallocating
against it, verifying against it, or reporting it after a save — would be
building on a number Messages does not guarantee.

## 9 What slice 3 spends, and what it does not

The daemon opens one file outside `chat.db`, and only one: the file named by the
`filename` of the rowid it was handed. Everything else about the rule in §3 is
unchanged. The caller cannot name a path, in either direction — it does not say
which file to read, and it does not say where to write, because **the client
writes the file itself**. The daemon hands over bytes and never touches the
destination, which is the same shape sending already uses in reverse.

That symmetry is worth stating plainly, because it is what keeps the daemon from
becoming an arbitrary-file *writer* to go with the arbitrary-file reader it
already refuses to be.

Streamed in 4MB chunks rather than returned whole. §2 measured a median
attachment of 1.9MB and a largest of 548MB; as a single base64 field that last
one would cost the better part of a gigabyte in the daemon and again in the
client, so the files most worth exporting would be the ones that failed. Peak
memory is one chunk instead of one file.

The client writes to a temporary name in the destination directory and renames
at the end, so an interrupted transfer leaves nothing that looks like a whole
file. An existing file is refused rather than replaced, unless `--force`.
