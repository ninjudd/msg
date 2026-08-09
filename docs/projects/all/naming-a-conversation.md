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

**Falling back to a unique superset is worth considering and is not decided.**
If no group has exactly those members but exactly one contains all of them, that
is arguably what was meant, and it is the difference between naming two people
out of a room of six and having to type all six. The counter is that it makes
`msg chat` answer with a conversation containing people nobody named. Leave it
out of the first slice; the failure without it is a clear "no conversation with
exactly those people", which is a good error to have.

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

The rename in `next.md` — `msg read` becoming `msg chat` — is independent of
both and can land in any order. This plan writes `msg chat` throughout on the
assumption it lands first; if it does not, the same behaviour belongs on
`msg read` under its current name.

## 7 What this is not

- **Not a change to merging.** A person's direct conversation is still all of
  their threads merged, per `conversation-merging.md`. §3 decides which
  conversation is meant, not what it contains.
- **Not a search feature.** `--with` and `--from` already take a person and
  already span their addresses. This is about naming the conversation to read.
- **Not fuzzy matching.** A misspelt name finds nothing, as now.
