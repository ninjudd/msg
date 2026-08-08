# Later

Wanted, nobody has committed to it. One line each, same rules as [now](now.md).
See [README](README.md) for how these lists work.

- [Stop `msg search` reading every message](all/query-performance.md) — the chat
  list is fixed; search still takes about 1.5s, because matching `attributedBody`
  cannot use an index.
- [What the signing identity costs, and what to fix before this is public](all/signing-identity.md)
  — the key that keeps the grant across rebuilds is also a way to re-grant
  silently.
- Read attachments, which currently show only as the U+FFFC placeholder they
  occupy in the message body.
- Export a conversation to a file, once there is a second consumer that wants
  more shape than `--json` piped through `jq`.
- Group messages by thread (`thread_originator_guid`), so replies read as
  replies rather than as flat chronology.
