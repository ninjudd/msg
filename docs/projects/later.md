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
- Read attachments, which currently show only as the U+FFFC placeholder they
  occupy in the message body.
- Export a conversation to a file, once there is a second consumer that wants
  more shape than `--json` piped through `jq`.
- Group messages by thread (`thread_originator_guid`), so replies read as
  replies rather than as flat chronology.
