# Project plans

This directory follows the [Projector](https://github.com/ninjudd/projector)
convention. Store each project in a permanent directory under `docs/projects/`.
Use a lowercase `readme.md` entry point with YAML frontmatter carrying two
fields: `status: draft|ready|in-progress|completed` records the lifecycle, and
`priority: now|next|later` records when the work should happen. Priority is
required unless the status is `completed`. Nest a project directory inside
another project when the work is a subproject. Keep supplemental files beside
the entry point that owns them.

Status and priority changes edit frontmatter. Do not create shared queue
files, status or priority directories, or symlinks, and do not move a project
when its status or priority changes. Number plan sections and never renumber
them after another document or code comment cites them.

Run `project list` to browse projects and `project check` to validate the
tree. Both commands come from the Projector CLI:

```sh
pipx install git+https://github.com/ninjudd/projector.git
```

## Conventions this repository adds

Open a plan with a `# Title`, a `**Status:**` line, and a `**Goal:**` line:

```markdown
---
status: draft
priority: next
---

# Plan: A daemon, so the terminal stops holding Full Disk Access

**Status:** Designed, not started. `msg` currently requires Full Disk Access on
the terminal, which is what this replaces.

**Goal:** Move the privileged read into a launchd agent that holds Full Disk
Access on its own.
```

The frontmatter carries the claim, and the `**Status:**` line carries the
story: what has landed, what is left, and what the plan waits on. A keyword
cannot say that. Update both when the state changes. A plan whose status says
"Designed" a month after shipping is worse than no status at all, and a stale
keyword is worse again, because reviews read it. The goal states the outcome in
a sentence or two, so a reader who stops after it still knows what the work
delivers.

`draft` is load-bearing: it declares that the plan is not yet claiming to be
executable, so open questions in a draft plan do not block its pull requests
from merging. The pull request that moves a plan to `ready` or `in-progress` is
making the readiness claim, and it answers for every question still open at
that moment.

A project is finished when its last pull request merges. That pull request
sets `status: completed`, and the plan states in an `**Outcome:**` line whether
it shipped, was abandoned, or was superseded. When a finished plan leaves a
follow-up worth scheduling, give the follow-up its own project and link it from
the outcome line, so it keeps a priority of its own.

Record decisions with the alternatives they beat. Mark a settled one
`(DECIDED)` and keep the rejected options and the reason each lost. Half of
what these plans are read for later is why the obvious cheaper thing was not
done. That matters more than usual here, because several of the security-shaped
conclusions are the opposite of the intuitive answer and look like oversights
to anyone reading only the outcome.

Correct the plan when something proves it wrong. A finding against a plan gets
fixed in the plan, noting what it replaced, the same as a finding against code.

Cite a plan by section using the project name and a `.md` suffix, for example
`daemon-and-permissions.md §5`, which names
`docs/projects/daemon-and-permissions/readme.md`. The shorthand predates the
move to permanent directories and survives it deliberately: the plans and the
Rust sources carry dozens of these citations, and rewriting them to spell out
`/readme.md` would change every one of those files to say what the short form
already says. Write a link beside the citation when a reader needs to follow
it. A citation states the behaviour and the section holds the reasoning behind
it, so find the citations of a plan before you touch its sections:

```sh
rg -n 'daemon-and-permissions.md §' src docs scripts README.md AGENTS.md
```

A project with no plan yet is still a project. Give it a directory and a short
`readme.md` that records the idea and why it matters, rather than a line in a
shared list.

Not everything here is a plan. Post-mortems, decision logs, and reference notes
belong in a project's directory too.

## What does not go here

How-it-works documentation stays in [README.md](../../README.md), which
describes current behaviour for someone using the tool. These plans are
point-in-time execution artifacts: once the work has landed, the code and the
README are the source of truth, and a finished plan is read for the *why*
behind a design and the constraints it was built under. Do not rewrite a
finished plan to match later reality.
