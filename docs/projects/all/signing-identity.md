# Plan: What the signing identity costs, and what to fix before this is public

**Status:** Partly acted on. The two cheapest options in §6 have since landed —
the README says what the build creates and how to remove it, and `msg daemon
uninstall` prints the `security delete-identity` line beside the `tccutil` one.
The trade in §2 stands as described and nothing has been done about it.

**Previously:** written down, nothing changed. The identity ships today: the first
`pnpm build:msgd` creates a self-signed `msg dev` certificate in the login
keychain and signs `msgd` with it. Nothing here is a defect report — it works —
but the cost has not been written down anywhere, and one of the gaps below is
the kind of thing that reads as carelessness rather than as a trade.

**Goal:** Decide what the signing key is allowed to cost, and close the gaps
that would make its creation look like a side effect nobody thought about.

## 1 Why the key exists at all

An ad-hoc signature is matched by cdhash, so every rebuild of `msgd` invalidates
its Full Disk Access grant and it has to be added again by hand. A stable
signing identity anchors TCC's requirement to a certificate instead, and the
grant survives rebuilds. That is the whole of the benefit, and it is a real one:
without it, iterating on the daemon means a trip to System Settings per build.

See [daemon-and-permissions.md §4](daemon-and-permissions.md).

## 2 The key is a way to re-grant, silently

The grant now says, in effect, "anything signed by this certificate, under this
identifier". So whatever can use the key can produce a binary that satisfies it.
`~/.local/libexec/msgd` is user-writable, so local code that can use the key can
replace the daemon with its own and inherit Full Disk Access without a prompt.

| | Hostile same-uid code gets |
| --- | --- |
| Daemon signed ad-hoc | the messages, via the daemon's API |
| Daemon signed by a key it can use freely | the whole disk, by signing its own |

That second row is the scope reduction in
[daemon-and-permissions.md §6](daemon-and-permissions.md) being handed back. It
is worth stating plainly, because the benefit in §1 and the cost here are the
same mechanism seen from two sides.

**What stops it is the keychain prompt.** The key is imported with an ACL naming
`codesign` and *without* `security set-key-partition-list`, so macOS asks before
each use. Answering that prompt with "Always Allow" — or setting the partition
list — trades the property away for a quieter build. This is the same conclusion
[§5](daemon-and-permissions.md) reached for the socket: against an attacker
already running as the user, user presence is the only barrier that survives.

The uncomfortable part is that the convenience knob and the security knob are
the same knob, and the one that quiets the prompt is the one a frustrated person
reaches for.

## 3 What it leaves behind

- The certificate is valid for **ten years** and lives in the login keychain.
- `msg daemon uninstall` does not remove it, and does not mention it. Neither
  does anything else. `security delete-identity -c "msg dev"` is the cleanup,
  and nobody will guess that.
- It is self-signed and untrusted (`CSSMERR_TP_NOT_TRUSTED`). That is fine for
  signing — measured, `codesign` exits 0 — but it means the binary is not
  distributable to anyone else, and `codesign --verify` against a trust policy
  will fail. Worth saying out loud so nobody assumes otherwise.
- It is per-user and per-machine. A second machine mints its own certificate
  with a different leaf, so grants do not travel.

## 4 It is a build step, not an install step

Worth being precise, because the distinction is exactly what people object to:

- `pnpm install` runs `prepare`, which runs `build`, which is `tsc`. **It never
  touches the keychain.**
- `pnpm build:msgd` is what creates the certificate, and only when one named
  `msg dev` is not already there.

So this is not a postinstall script rummaging in your keychain. It is an
explicit build command doing something surprising, which is a smaller sin and
still worth announcing.

## 5 How this will read in public

Ranked by how likely it is to come up, and how much it would sting:

1. **"115MB for an iMessage CLI."** Certain, and funny. The answer — a Single
   Executable Application is a copy of the `node` binary — is correct and will
   not stop anyone saying it.
2. **Hand-patching a Mach-O to inject `__TEXT,__info_plist` into a copy of
   node.** Splits the room between "great writeup" and "cursed, and it breaks the
   next time Node moves something". Both fair. It is also the most interesting
   thing here, so it should be led with rather than buried.
3. **A build script minting a code signing certificate in the login keychain.**
   The only item that can land as a real criticism rather than a joke, and the
   only one where the criticism would be right. It is also the cheapest to
   defuse: say it, document removing it, keep the opt-out.
4. **"Nothing should ever need Full Disk Access."** Reflexive, and a fraction of
   readers stop there without reaching the part where the entire design exists to
   narrow that grant. The README leading with the comparison is the best
   available defence.
5. **Someone derives §2 independently.** The sharpest reader works out that the
   signing key re-widens the grant. Having written it down first turns that from
   a gotcha into a nod — which is the same reason
   [daemon-and-permissions.md §5](daemon-and-permissions.md) records why the
   socket is deliberately unauthenticated.

The pattern across all five: the things that get mocked are the things that are
*visible*, and the things that get respected are the trades that were written
down before anyone asked. The plan documents are the asset here, not the code.

## 6 Options

Not decided. Roughly in order of how much they cost:

- **Announce it.** *Done.* The README now says the first build creates the
  certificate, and that answering the keychain prompt with "Always Allow"
  trades away what §2 relies on.
- **Document removal.** *Done.* `msg daemon uninstall` prints the
  `security delete-identity` line beside the `tccutil` one, since grants and
  keys both outlive the binary.
- **Make creation opt-in.** A first build could refuse to create anything and
  print the two choices — `MSG_SIGN_IDENTITY=-` for ad-hoc and a re-grant per
  build, or a flag to create the identity. Costs one round trip the first time,
  and removes the "it did *what*?" reaction entirely.
- **A dedicated keychain that stays locked** except while building. Defence in
  depth against §2, at the cost of an unlock step and a lot of explaining.
  Probably not worth it, recorded so the next person does not have to work out
  why it was skipped.

## 7 What is unresolved

- ~~Whether a rebuild actually keeps the grant with this certificate.~~
  **Measured, 2026-08-07.** The grant was given once to a `msg dev`-signed
  binary and survived eight rebuild-and-reinstall cycles without being touched
  again, including builds that changed the embedded `Info.plist` and the code.
  Full Disk Access kept answering throughout. The earlier measurement in
  [§9](daemon-and-permissions.md) used an Apple Development identity; this one
  used the certificate the build actually creates.
- The exact requirement TCC stored. The behaviour — cdhash changes, grant holds
  — is measured; the `csreq` blob itself has never been dumped, so "anchored to
  the certificate" is inference from behaviour rather than from the record.
- Whether an expired certificate takes the grant with it in ten years, or
  whether TCC keeps honouring it. Nobody will remember to test this.
