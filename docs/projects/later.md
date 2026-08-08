# Later

Wanted, nobody has committed to it. One line each, same rules as [now](now.md).
See [README](README.md) for how these lists work.

- [Index message bodies ourselves](all/search-index.md) — nothing else indexes
  them: Spotlight does not, `chat.db` does not, and BlueBubbles injects a dylib
  into Messages.app to avoid the problem. A partial index still helps, because a
  search can be bounded by date.
- [What the signing identity costs, and what to fix before this is public](all/signing-identity.md)
  — the key that keeps the grant across rebuilds is also a way to re-grant
  silently.
- Read attachments, which currently show only as the U+FFFC placeholder they
  occupy in the message body.
- Export a conversation to a file, once there is a second consumer that wants
  more shape than `--json` piped through `jq`.
- Group messages by thread (`thread_originator_guid`), so replies read as
  replies rather than as flat chronology.
