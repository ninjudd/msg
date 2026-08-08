# Later

Wanted, nobody has committed to it. One line each, same rules as [now](now.md).
See [README](README.md) for how these lists work.

- [Make the common commands stop taking two seconds](all/query-performance.md)
  — `chats` and `read` are around 2s on a real database, and the `LIMIT` does
  not reduce the work.
- [What the signing identity costs, and what to fix before this is public](all/signing-identity.md)
  — the key that keeps the grant across rebuilds is also a way to re-grant
  silently.
- Read attachments, which currently show only as the U+FFFC placeholder they
  occupy in the message body.
- Export a conversation to a file, once there is a second consumer that wants
  more shape than `--json` piped through `jq`.
- Group messages by thread (`thread_originator_guid`), so replies read as
  replies rather than as flat chronology.
