# Later

Wanted, nobody has committed to it. One line each, same rules as [now](now.md).
See [README](README.md) for how these lists work.

- [Search backwards through time and stream the results](all/query-performance.md)
  — §9. Not urgent: search is around 2.5s unscoped and 236ms scoped to a person,
  which was judged fast enough.
- [Index message bodies ourselves](all/search-index.md) — would make search
  instant, and is the only way to, since nothing else indexes message bodies.
  Deliberately not being built: see §7 of that plan for what it would cost, which
  is more than the current speed is hurting.
- [What the signing identity costs, and what to fix before this is public](all/signing-identity.md)
  — the key that keeps the grant across rebuilds is also a way to re-grant
  silently.
- [Get attachment bytes out, by rowid](all/attachments.md) — §6 slice 3, the one
  that makes the daemon open a file for a caller who cannot. §3 is the thinking
  it has to satisfy first.
- [Group a conversation by thread](all/threading.md) — §5 slice 2, so a thread
  reads together instead of scattered through the transcript. Slice 1 is what
  makes the case for it or against it.
- Export a conversation to a file, once there is a second consumer that wants
  more shape than `--json` piped through `jq`.
