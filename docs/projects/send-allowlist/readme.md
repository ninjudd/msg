---
status: completed
---

**Outcome:** Shipped.

# Plan: Send without asking, but only to people you listed

**Status:** Designed and implemented in one pull request. The unit and daemon
tests cover every refusal and prove a listed address gets through (§8). A
live send depends on the author's install and is described in that pull
request's Testing section.

**Goal:** `msg send --listed` sends only to addresses on a list that only
root can change. Because the flag can reach nobody else, an agent may be
approved to pass it in advance, and so can text the people you chose without
asking you each time.

## 1 Why

The two gates on sending answer "may this tool text people at all?" Once
both are open, the only thing standing between an agent and any person in
Contacts is the Agent Skill's rule never to send unless asked, plus the
agent's own permission prompt. Neither can be relaxed for one recipient
without being relaxed for all of them. An allow rule for `msg send` in an
agent's settings approves texting anyone.

The request that prompted this was narrower: let an agent text a few
specific people, such as family, without asking first, and keep asking
about everyone else. That needs a limit that holds when the agent is wrong.
The agent reads incoming messages through this same tool, and any
message's text can be written to steer it, so the agent can be wrong on
purpose. The limit therefore has to live where the agent cannot move it.

[confirmed-and-delayed-send.md](../confirmed-and-delayed-send/readme.md) is
the other answer to the same problem: a person approves every send with
Touch ID. The two approaches complement each other. That plan makes every
send safe to start. This one makes some sends safe to finish unattended,
and it needs no new platform surface.

## 2 A flag the caller passes, enforced by the daemon (DECIDED)

`msg send --listed <to> <body>` asks the daemon to refuse the send unless
every member of the resolved conversation is on the list. Without the flag,
`send` behaves exactly as before.

An agent's permission system can match the flag, so a person can approve
`msg send --listed *` in advance while every other `msg send` still asks.
That approval stays safe on any machine. Where no list exists, `--listed`
refuses everything (§3), so a settings file shared across machines cannot
turn into "text anyone" on the one that never set up a list.

Rejected: a config key that restricts every send once a list exists. This
key could not be approved in advance anywhere. An allow rule for plain
`msg send` would text anyone on a machine without a list, or on one whose
list was removed, so the agent's settings would have to repeat the list
address by address, and the two copies would drift apart.

Rejected: checking the list in the CLI. A check the caller runs on itself
is advice (daemon-and-permissions.md §7). The CLI does check under
`--dry-run`, so the preview says what the send would say, but the daemon
checks again before it sends.

## 3 Only root can widen the list (DECIDED)

The list is `/etc/msg/allowlist`. The daemon reads it only if the file and
every directory above it are owned by root and writable by nobody else. It
takes the owner and mode from the open file, not the path, so a file
swapped in after the check is the file that gets checked. A list that is
missing or fails the check admits nobody, and the refusal says how to fix
it.

The process being limited runs as the user, so any list the user can write
is a list that process can extend. Adding someone with `sudo` requires a
password or Touch ID, which a process the agent starts cannot supply. This
is the rule from confirmed-and-delayed-send.md §14, reached without the
Keychain: a gate the adversary can only tighten is a gate.

The path is a constant. The config path follows `MSG_CONFIG`, but the list
path follows nothing in the environment, the launchd plist, or the wire. A
path the caller can choose leads to a list the caller can write. Tests pass
a `Location` through `DaemonOptions` and name their own uid as the owner,
because no test can create a root-owned file.

Rejected: keeping the list in `~/.config/msg/config.toml`, next to
`send = true`. That file is fine for a switch that prevents accidents. A
limit that the limited process can edit is not a limit.

Rejected: one list per user, such as `/etc/msg/allowlist.d/<user>`. Nobody
has asked for a Mac with several people each running `msg`. It can be
added if someone does.

## 4 What the list holds, and what it admits (DECIDED)

The list holds one address per line: an email address, or a phone number
with its country code. `#` starts a comment and blank lines are skipped.
Any other line makes the whole list an error that names the line, so a
typo is reported instead of quietly admitting nobody.

Names are not accepted. A name resolves through Contacts, and `msg`
itself can edit Contacts (contact-writing.md), so a listed name would admit
whoever was renamed to it.

An address matches exactly, not on its last ten digits the way
`contacts::handle_key` matches when finding a person. That rule would let
`+13105551234` admit `+443105551234`. Email addresses are compared without
case. Phone numbers drop the punctuation `contacts::emit_phone` already
strips for Contacts, and they reuse that function rather than a second
copy of its character set.

The check runs against the members of the conversation the send resolved
to, so `dana` resolves to one of Dana's routes and that route's address has
to be listed. A room is admitted only when every member is listed, and the
refusal names the members who are not. A chat guid is refused under
`--listed`, because the daemon sends to a guid without reading who is in
it. An address or a name is what the flag takes.

## 5 The list is checked before the config key (DECIDED)

Under `--listed`, the daemon resolves the conversation, checks the list,
and only then reads `send = true`. The conversation is resolved once, and
the chat that was checked is the chat that is sent to.

The order makes a listed address testable. The test daemon never enables
sending, so an admitted address reaches the closed gate and stops, and the
`SendDisabled` refusal proves it was let through. When both would refuse,
the error names the list, which is the more specific reason.

## 6 The wire

`send` gains `listed`, and the protocol version goes from 19 to 20. A stale
daemon would ignore the unknown field and send to whoever was named, so the
one flag trusted to run unattended would reach anyone. The version gate
turns that into "reinstall the daemon".

## 7 What it changes for agents

The Agent Skill keeps "never send unless asked" for every plain `msg send`.
It adds one exception: a person who created the list has approved everyone
on it in advance, so when the task calls for a text to someone listed, the
agent sends it with `--listed` rather than stopping to ask. The agent
writes the flag right after `send`, which is where a prefix permission rule
can match it. Enabling the gates, and adding people to the list, remain the
user's to do.

For Claude Code, the matching rule is
`"permissions": {"allow": ["Bash(msg send --listed *)"]}`. The rule is
narrow enough that auto mode keeps it. Do not add an `ask` rule for
`msg send *` next to it, because `ask` outranks `allow`.

## 8 Acceptance and what proves it

- **A listed address gets through, and nothing else does.** The tests in
  `tests/daemon.rs` under `send --listed` show that a listed number, and a
  listed email typed in another case, reach the closed config gate. An
  unlisted address, a room with one unlisted member, and a guid are all
  refused before the gate. With the call to `allowlist::check` removed from
  the handler, the refusal tests fail, so they guard the gate itself.
- **Only root can widen the list.** The unit tests in
  `src/daemon/allowlist.rs` refuse a list owned by the wrong uid, a list
  that others can write, a list in a directory others can write, and a
  missing list. They also show that a number without its country code, and
  one that only ends in the same digits, are not admitted.
- **The flag changes nothing without it.** An unlisted address sent without
  `--listed` still meets the config gate first, as before.
- **Cost does not matter here.** A `--listed` send reads one small file. It
  runs at the rate a person sends texts, behind an `osascript` launch that
  costs far more.

## 9 What it does not limit

The list limits who a send reaches, not what it carries. An agent that is
approved for `--listed` can send a listed person any text or `--file`. The
person who writes the list is deciding who may receive whatever an agent
sends. confirmed-and-delayed-send.md §6 is the plan for approving content.
