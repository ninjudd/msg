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
- [Show one thread on demand](all/threading.md) — §8. Not regrouping the
  transcript, which is deliberately not being built; showing every message in a
  thread given any message in it. Needs message ids to be visible first, which
  is the same problem attachments solved by printing theirs.
- [Resolve the person before matching chat rows](all/resolver-windows.md) — §3.
  The remaining latent half: the resolver enters through a scan of the newest
  5,000 chat rows, so past 5,000 chats a long-quiet person falls out of reach
  before the person lookup runs. Cannot bite below 5,000; this database holds
  1,165.
- Export a conversation to a file, once there is a second consumer that wants
  more shape than `--json` piped through `jq`.
