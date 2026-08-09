# Plan: Name a conversation by who is in it

**Status:** Not started. Depends on nothing, but overlaps
`search-boundaries.md` — §2 here is partly fixed for free by the word-start rule
that plan describes, and §7 of that plan is corrected as a result: the case it
was waiting for has now been observed.

**Goal:** Let a bare first name reach that person's conversation, and let
several names reach the group with exactly those people in it, so
`msg chat dana` and `msg chat dana sam` both mean something obvious.

## 1 What happens today

A chat is named by rowid, by a handle, or by any substring of its name, and
anything matching more than one conversation is reported as an ambiguity with
the candidates listed. That is the right default and it is what makes a first
name almost unusable: a first name matches every group the person is in, as well
as their own conversation.

Measured on a real database, one four-letter first name matched **eight**
conversations — two direct conversations with the person, four groups they are a
member of, and two conversations belonging to entirely different people. The
command printed six of the eight and said "and 2 more", which is a correct
answer to a question nobody asked.

## 2 Two of those eight were not matches at all

The last pair is a different bug wearing the same coat. Name matching is a plain
substring test, so a needle lands inside longer names: `ana` matches `Ana
Duarte`, which is wanted, and also `Dana Reyes` and `Susana Vidal`, which are
not.

**Name matching takes the word-start rule too. (DECIDED)** That is exactly
`search-boundaries.md`, whose §7 scoped itself to message bodies and said to
leave name matching alone until the noise showed up there. It has now shown up,
so §7 is corrected to say so, and the rule transfers unchanged: an occurrence
qualifies when the character before it is not alphanumeric. A needle must begin
where a name begins.

The two should share one predicate rather than growing a second definition of
what a word start is — which `search-boundaries.md §3` already anticipated when
it asked for the boundary-aware function to take `&str`, since a contact name
is one.

**This plan does not depend on that one**, and neither blocks the other. But the
candidate list shrinks by a quarter for free once the boundary rule lands, and
every example below is cleaner in a world where it has.

## 3 One name means that person's own conversation

When a name resolves to exactly one *contact*, the answer is that contact's
direct conversation, whatever else the substring also touched. Groups they are a
member of do not compete with it: being in a room is not being the person, which
is the distinction `sole_person` already draws and the one `resolve_chat`'s
comment already relies on.

That extends the rule `conversation-merging.md §5` settled rather than replacing
it. A rowid still means one thread. A name still means the person — this only
says that a person's conversation is their direct one, and that a group
containing them is not a rival reading.

**When the person has no direct conversation**, there is nothing to prefer, and
the groups are the honest answer. List them as today rather than inventing a
conversation that does not exist.

**When the name resolves to two contacts**, the ambiguity is real and stands.
Two people who share a first name are two people, the same argument that keeps
identity on the Contacts record rather than on the rendered name.

## 4 Several names mean the group with those people in it

`msg chat dana sam` is the conversation whose members are exactly Dana and Sam
and me. Each argument names a person, resolved the same way one argument is, so
first names work as soon as they are unambiguous.

**Exactly those people, not at least them. (DECIDED)** A superset rule makes
`dana sam` match every larger room the two share, which is an ambiguity again
and a worse one, because the candidates all look equally right. Exact membership
gives one answer or none.
*Rejected:* preferring the smallest superset, which silently answers with a
different room than the one asked for as soon as the exact one does not exist.

**No fallback to a unique superset. (DECIDED)** If no group has exactly those
members, the answer is that there is no such conversation, even when exactly one
room contains all of them. The tempting argument is convenience — naming two
people out of a room of six beats typing all six — and it loses to the same
principle the rest of this plan turns on: a command may not answer with a
conversation containing people nobody named. "No conversation with exactly those
people" is a good error, and it is one the user can act on by naming the rest.
*Rejected:* the unique-superset fallback, which is silent when it is wrong and
indistinguishable from a hit when it is right.

**A named group is still reachable by its name.** `msg chat "Ship Room"` does
not change. Membership naming is for the unnamed rooms, which is most of them.

## 5 The sharp edge: a space is an argument separator

`msg chat ana duarte` is two people under this rule, not one person called Ana
Duarte. The answer is quoting — `msg chat "Ana Duarte"` — and it is worth
stating plainly in `--help` rather than discovered.

It is not a regression, because a multi-word name has to be quoted today anyway:
the argument is a single positional, so `msg read ana duarte` is already an
error. What changes is that the unquoted form stops erroring and starts meaning
something else, which is the more dangerous shape of the same edge.

**Quoted and unquoted must not silently disagree about a real conversation.**
Worth a test with a two-word contact name and a two-person group whose members'
first names are those two words, if such a case can be constructed, since that
is where the two readings collide on live data rather than in principle.

## 6 Slices

**One: a single name prefers the person.** §3 only. This is the reported
annoyance and it is coherent alone.

**Two: several names name a group.** §4, exact membership. Needs a way to
resolve N person specs to a chat by membership, which `chat_handle_join` answers
directly — the same table `one_to_one_chats` already asks about, with the count
clause turned into an exact-set comparison.

Slice one is where §8's unification belongs, because it is the slice that makes
`resolve_conversation` ask about a person rather than about chat rows. Doing it
then costs nothing extra; doing it later means writing the person-preference
rule twice and deleting one of them.

The rename in `next.md` — `msg read` becoming `msg chat` — is independent of
both and can land in any order. This plan writes `msg chat` throughout on the
assumption it lands first; if it does not, the same behaviour belongs on
`msg read` under its current name.

## 7 What this is not

- **Not a change to merging.** A person's direct conversation is still all of
  their threads merged, per `conversation-merging.md`. §3 decides which
  conversation is meant, not what it contains.
- **Not a change to what `--with` and `--from` select.** They are in scope only
  as callers of §8's primitive, so that a name resolves identically whether it
  is being read or searched. Which messages each of them then returns is
  unchanged, and the open question at the end of §8 is deliberately left open.
- **Not fuzzy matching.** A misspelt name finds nothing, as now.

## 8 One primitive for finding a person

Everything above needs the same operation — a spec in, one person out — and so
does `--with`, `--from`, `msg contacts`, and sending. There must be one
implementation of it, because two that disagree about who somebody is would be
the subtle bug `conversation-merging.md §3` already argues against for identity.

**`resolve_person` is that primitive and most of it already exists.** It takes a
spec and answers with one `Person`, spanning every address a Contacts record
holds. An address typed in full matches on `handle_key`, so its written shape
does not matter, and only when no address matches outright is the spec read as a
name. It already honours nicknames in both directions: `answers_to` matches the
displayed nickname and the filed name it displaced, and an exact match on either
breaks a tie, because typing the whole of one is as definite as typing the whole
of the other.

**What is wrong today is that it is not the only one.** `resolve_conversation`
does not call it. It matches chat rows independently — `displayName`,
`chatIdentifier`, `handles`, plus `any_named` against the contact index — so
reading and searching resolve a name by two different routes that can disagree.
§3's rule is the occasion to collapse them: find the person first, then find
their conversation, rather than asking the chat table to answer a question about
people.

**It errors rather than guessing. (DECIDED)** A spec that resolves to nobody,
or to more than one person, is an error and stops the command. `--with dana`
where Dana cannot be found must not quietly search every conversation, which is
the failure mode that makes a filter worse than no filter: the results look like
an answer. `resolve_person` already fails this way — "no one matching" — and the
requirement is that every caller keeps it rather than falling back.

**The word-start rule from §2 lives here**, in the one place, which is what
makes "shared primitive" worth more than a tidiness argument: the boundary rule
gets written once and every caller gets it.

### What `--from` and `--with` currently mean, which is worth stating before it changes

They differ by more than scope today, and the difference is deliberate:

| | Their messages | My messages |
| --- | --- | --- |
| `--from` | every conversation they appear in | never |
| `--with` | every conversation they appear in | only in a one-to-one with them |

`Sender::BothWays` excludes my messages in a group on purpose — they were
addressed to the room, not to that person, so counting them would return most of
my own history from any busy group.

**Whether `--from` should also narrow to the direct conversation is open.**
"Search my conversation with them" and "search what they said, wherever they
said it" are both reasonable readings of `--from`, and they differ for anyone
sharing a group with the person. Nothing here changes it; the shared primitive
is about resolving *who*, and this is a question about *where*, which should be
settled on its own rather than as a side effect.