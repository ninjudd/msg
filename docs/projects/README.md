# Projects

Work in progress lives here, in git, next to the code it describes.

## How it works

One directory of documents, and three short lists that point into it.

| | What it is |
| --- | --- |
| [Now](now.md) | What's actively being worked on. |
| [Next](next.md) | Coming up, not started yet. |
| [Later](later.md) | Wanted, nobody has committed to it. |
| [All](all) | Every project document, whatever its state. Nothing here ever moves. |

**A list entry is one line.** Name the work, link its plan if it has one, and
add at most a sentence of why it matters. The moment the line wants a second
sentence it wants a document instead. A line without a document is fine and
common; a paragraph sitting in a list is not.

Work flows down the lists as it gets picked up, [Later](later.md) →
[Next](next.md) → [Now](now.md), and off the end when it's done. It flows back
up too: an item that turns out bigger than expected moves to [Later](later.md)
whole, plan and all. A project is past by **not being on any of the three
lists**. There is no `done/` directory, so there is nothing to move when work
finishes — you delete a line from `now.md`.

Because documents never move, `docs/projects/all/<name>.md` can be linked from
code comments, other docs, or a commit message and trusted to keep working.

## Sections are part of the interface

Plans are cited by section — `daemon-and-permissions.md §5` — because a comment
states the behaviour and the section holds the reasoning behind it. Renumbering
or removing a section breaks references no compiler will catch. Add sections at
the end, or grep for the citation before you renumber:

```
grep -rn "daemon-and-permissions.md §" src docs
```

## What goes in a document

Frontmatter first — `status:` between two `---` lines — then a `# Title`, then
a `**Status:**` line, then a `**Goal:**` line:

```markdown
---
status: Draft
---

# Plan: A daemon, so the terminal stops holding Full Disk Access

**Status:** Designed, not started. `msg` currently requires Full Disk Access on
the terminal, which is what this replaces.

**Goal:** Move the privileged read into a launchd agent that holds Full Disk
Access on its own.
```

`status:` is one keyword from a fixed set shared across repos (adopted from
`triangle/app`; see ninjudd/agent-config#15): `Draft` (written, implementation
not started), `Active` (in progress), `Blocked` (waiting on a dependency or
decision), `Stalled` (lost momentum, not formally dropped), `Shipped`,
`Superseded`, `Abandoned`, `Reference` (a standing document with no build
lifecycle). No `owner:` field here — this repo has one person to ask.

The keyword does not replace the prose status line, because the useful thing to
say is still what has landed and what is left, and a keyword cannot say it. The
frontmatter carries the claim and the prose carries the story; keep both
current when the state changes — a plan whose status says "Designed" a month
after shipping is worse than no status at all, and a stale keyword is worse
again, because reviews read it.

`Draft` is load-bearing: it declares the plan is not yet claiming to be
executable, so open questions in a Draft plan do not block its pull requests
from merging. The pull request that flips a plan out of Draft is making the
readiness claim, and it answers for every question still open at that moment.

Then write what the work needs. Two habits worth keeping:

- **Record decisions with their alternatives.** `(DECIDED)` with the rejected
  options and why beats a plan that only states the outcome. Half of what these
  documents are read for later is why the obvious cheaper thing was not done.
  This matters more than usual here, because several of the security-shaped
  conclusions are the opposite of the intuitive answer and will look like
  oversights to anyone reading only the outcome.
- **Correct the plan when something proves it wrong.** A finding that lands on a
  plan gets fixed in the plan, noting what it replaced, the same as a finding
  against code.

Not everything here is a plan. Post-mortems, decision logs, and reference notes
belong in a project's document too, which is why the directory is `all/` and not
`plans/`.

## What doesn't go here

How-it-works documentation stays in [README.md](../../README.md), which
describes current behaviour for someone using the tool. These are point-in-time
execution artifacts: once the work has landed, the code and the README are the
source of truth, and a finished project document is read for the *why* behind a
design and the constraints it was built under. Don't rewrite a finished plan to
match later reality.
