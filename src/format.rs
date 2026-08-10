//! Rendering for terminal and JSON output.

use chrono::{DateTime, Datelike, Local, Utc};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::db::{Chat, Message};

/// `9:35 AM` and `Jan 15, 9:36 AM`.
///
/// The TypeScript build asked `Intl.DateTimeFormat` for these with the system
/// locale, so on a machine set to another one they came out in that locale.
/// There is no ICU here and pulling one in for two format strings is not worth
/// 10MB, so these are fixed. On this machine the output is identical to what
/// `Intl` produced; on a non-English machine it is now English.
const TIME: &str = "%-I:%M %p";
const DATE_TIME: &str = "%b %-d, %-I:%M %p";

fn local(date: DateTime<Utc>) -> DateTime<Local> {
    date.with_timezone(&Local)
}

fn is_today(date: DateTime<Local>) -> bool {
    let now = Local::now();
    date.year() == now.year() && date.month() == now.month() && date.day() == now.day()
}

pub fn format_timestamp(date: Option<DateTime<Utc>>) -> String {
    let Some(date) = date.map(local) else {
        return String::new();
    };
    if is_today(date) {
        date.format(TIME).to_string()
    } else {
        date.format(DATE_TIME).to_string()
    }
}

pub fn relative_age(date: Option<DateTime<Utc>>) -> String {
    let Some(date) = date else {
        return "never".to_string();
    };
    let seconds = (Utc::now() - date).num_seconds().max(0);
    match seconds {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        86_400..2_592_000 => format!("{}d ago", seconds / 86_400),
        _ => local(date).format(DATE_TIME).to_string(),
    }
}

/// How many terminal cells a string occupies.
///
/// Three counts disagree here and only one of them lines up a column. `len()`
/// is bytes, so `café` measures 5. `chars().count()` is scalar values, so `😀`
/// measures 1 where a terminal draws 2 — and that was this program's first
/// answer, which regressed emoji-named conversations that the JavaScript
/// build's UTF-16 `.length` had happened to get right. This is UAX #11, which
/// is the question actually being asked.
///
/// One class of character splits the answer: a VS16-promoted symbol like `❤️`
/// is two cells to this crate and one to the wcwidth most terminals still
/// follow, so the glyph draws a cell wider than the terminal advances. In the
/// transcript `spill` pads for that; in the chats column, a name carrying one
/// misaligns by that cell on such terminals — knowingly left rather than
/// unnoticed, until someone actually hits it (tapbacks.md §4 has the
/// mechanism).
fn width_of(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

/// Cut to `width` cells, leaving room for the ellipsis, which is one cell.
fn truncate(value: &str, width: usize) -> String {
    if width_of(value) <= width {
        return value.to_string();
    }
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for character in value.chars() {
        let cells = UnicodeWidthChar::width(character).unwrap_or(0);
        // A wide character that would straddle the boundary is dropped whole
        // rather than split, so the result never overruns by a cell.
        if used + cells > budget {
            break;
        }
        used += cells;
        out.push(character);
    }
    out.push('…');
    out
}

fn pad_end(value: &str, width: usize) -> String {
    let mut out = value.to_string();
    for _ in width_of(value)..width {
        out.push(' ');
    }
    out
}

pub fn render_chats(chats: &[Chat]) -> String {
    if chats.is_empty() {
        return "no chats found\n".to_string();
    }
    let width = chats
        .iter()
        .map(|chat| width_of(&chat.name))
        .max()
        .unwrap_or(0)
        .min(38);
    let ages: Vec<String> = chats
        .iter()
        .map(|chat| relative_age(chat.last_date))
        .collect();
    let age_width = ages.iter().map(|age| width_of(age)).max().unwrap_or(0);

    let mut out = String::new();
    for (chat, age) in chats.iter().zip(&ages) {
        let kind = if chat.is_group {
            format!("{} people", chat.member_count)
        } else {
            "direct".to_string()
        };
        out.push_str(&format!(
            "{:>5}  {}  {}  {kind}\n",
            chat.rowid,
            pad_end(&truncate(&chat.name, width), width),
            pad_end(age, age_width),
        ));
    }
    out
}

/// How reactions render on the messages they trail.
///
/// `Off` exactly when `--tapbacks` is showing reaction rows as their own
/// messages — printing both is the same information twice (tapbacks.md §6).
/// `Named` is `--who`: the same trail, each reaction naming its sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trail {
    Off,
    Symbols,
    Named,
}

/// The extra space a symbol needs when the terminal will draw over ours.
///
/// `❤️` and `‼️` are text-presentation characters promoted to emoji by VS16
/// (U+FE0F). The wcwidth most terminals still follow allocates them one
/// column while the glyph is drawn two wide, so the spill lands on whatever
/// comes next — which was the space before a sender's name. Detected by the
/// VS16 itself rather than by measuring: `unicode_width` 0.2 already counts
/// these as two, siding with what terminals should do over what the observed
/// ones did, so a width test reports the problem absent on exactly the
/// symbols that have it. Emoji-presentation defaults carry no VS16 and get
/// nothing.
fn spill(symbol: &str) -> &'static str {
    if symbol.contains('\u{fe0f}') { " " } else { "" }
}

/// How a list of messages is rendered, named because three positional flags
/// stopped reading at the call site.
#[derive(Debug, Clone, Copy)]
pub struct Render {
    /// Prefix each line with its conversation, for output that interleaves
    /// several — search and an unscoped watch.
    pub show_chat: bool,
    pub trail: Trail,
    /// A day line when the local day changes — [`day_header`]'s shape,
    /// `Today` through `Friday, July 7` — and only a time on each message:
    /// what Messages does. The chat transcript and the watch stream: a
    /// stream headers a day change too, since the header goes in front of
    /// the new line and the last emitted day is all it takes — `Renderer`
    /// holds that day across calls for exactly this. Search keeps its stamps
    /// as they were — `format_timestamp`, a date except on today's — because
    /// its results jump between days by construction.
    pub day_headers: bool,
    /// Wrap each date header in ANSI bold. Callers set this through
    /// [`styling_allowed`], so a pipe still sees plain text, grep still works,
    /// and a terminal that asked for plain gets it — the only control codes
    /// this program writes, and only where a human is looking and has not
    /// said no.
    pub styled: bool,
}

/// The transcript's date line: relative words while they are unambiguous,
/// then the date with its weekday, the year only when it is not this year —
/// `Today`, `Yesterday`, a bare weekday inside the last week, then
/// `Friday, July 7`, then `Friday, July 7, 2023`. English fixed, the same
/// choice TIME and DATE_TIME already made. Pure in `now` so every branch is
/// testable on any day the suite runs.
fn day_header(date: DateTime<Local>, now: DateTime<Local>) -> String {
    let days = (now.date_naive() - date.date_naive()).num_days();
    match days {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => date.format("%A").to_string(),
        _ if date.year() == now.year() => date.format("%A, %B %-d").to_string(),
        _ => date.format("%A, %B %-d, %Y").to_string(),
    }
}

/// Whether bold may be written: someone is looking, and nobody asked for
/// plain. `NO_COLOR` present and non-empty refuses styling (no-color.org, and
/// its constituency is real — screen readers, terminals that render SGR
/// badly, a session teed to a file); `TERM=dumb` is the older form of the
/// same request. Pure in its inputs so the rule is testable without touching
/// the process environment, which parallel tests cannot safely do.
pub fn styling_allowed(
    no_color: Option<&std::ffi::OsStr>,
    term: Option<&std::ffi::OsStr>,
    is_tty: bool,
) -> bool {
    if no_color.is_some_and(|value| !value.is_empty()) {
        return false;
    }
    if term.is_some_and(|value| value == "dumb") {
        return false;
    }
    is_tty
}

/// Rendering with the day held between calls, so a stream that prints one
/// message at a time still headers a day change. A batch caller uses
/// [`render_messages`]; a stream constructs one of these and keeps it.
pub struct Renderer {
    options: Render,
    last_day: Option<(i32, u32, u32)>,
}

/// One message, one line: a newline inside a body becomes a visible `↵`
/// instead of breaking the transcript's line-per-message shape, which the
/// context gutter, `-C` line counts, and grep over a transcript all rely on.
/// Gray when styling is on, so the mark reads as structure rather than as
/// text the sender typed; plain otherwise, and a pipe wants the one-line
/// property most and the escape-free property just as much. A trailing
/// newline run is trimmed rather than drawn — a mark that says only "the
/// body ended" marks nothing.
fn one_line(text: &str, styled: bool) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = unified.trim_end_matches('\n');
    if styled {
        trimmed.replace('\n', "\x1b[90m↵\x1b[0m")
    } else {
        trimmed.replace('\n', "↵")
    }
}

pub fn render_messages(messages: &[Message], options: Render) -> String {
    Renderer::new(options).render(messages)
}

impl Renderer {
    pub fn new(options: Render) -> Self {
        Self {
            options,
            last_day: None,
        }
    }

    pub fn render(&mut self, messages: &[Message]) -> String {
        let Render {
            show_chat,
            trail,
            day_headers,
            styled,
        } = self.options;
        if messages.is_empty() {
            return "no messages found\n".to_string();
        }
        // Context was asked for exactly when the messages carry a run to belong to.
        // Without it nothing below changes a byte of what this printed before.
        let in_context = messages.iter().any(|message| message.group.is_some());
        let mut out = String::new();
        let mut run: Option<i64> = None;
        for message in messages {
            if in_context {
                if let Some(group) = message.group
                    && run.is_some_and(|open| open != group)
                {
                    // grep's separator, for grep's reason: a blank line would be
                    // ambiguous against a message whose body is empty.
                    out.push_str("--\n");
                }
                run = message.group;
            }
            // Two columns before the timestamp rather than colour, so a hit
            // is still marked after the output has been piped, pasted, or
            // grepped again. The bold on a date header and the gray on a
            // folded `↵` are the styling exceptions this program makes, and
            // only when a terminal is looking and not refusing — piped output
            // still carries no control codes at all.
            let gutter = match (in_context, message.matched) {
                (false, _) => "",
                (true, true) => "> ",
                (true, false) => "  ",
            };
            if day_headers && let Some(date) = message.date.map(local) {
                let day = (date.year(), date.month(), date.day());
                if self.last_day != Some(day) {
                    let header = day_header(date, Local::now());
                    if styled {
                        out.push_str(&format!("\x1b[1m{header}\x1b[0m\n"));
                    } else {
                        out.push_str(&format!("{header}\n"));
                    }
                    self.last_day = Some(day);
                }
            }
            let stamp = if day_headers {
                message
                    .date
                    .map(|date| local(date).format(TIME).to_string())
                    .unwrap_or_default()
            } else {
                format_timestamp(message.date)
            };
            let where_ = match (show_chat, message.chat_name.as_deref()) {
                (true, Some(name)) => format!("[{name}] "),
                _ => String::new(),
            };
            let body = one_line(message.body.as_deref().unwrap_or("(no text)"), styled);
            // What is being answered goes above the answer, indented to the width of
            // a timestamp, so a reply reads as a reply without the transcript
            // stopping being chronological.
            if let Some(answering) = &message.reply_to {
                // Not folded: `excerpt` already flattened every run of
                // whitespace to one space, so there is no newline here to
                // mark and a fold call would be a dead path wearing a live
                // one's clothes.
                let quoted = answering.excerpt.as_deref().unwrap_or("(no text)");
                out.push_str(&format!(
                    "{gutter}{:width$}  ↳ replying to {}: {quoted}\n",
                    "",
                    answering.sender,
                    width = stamp.chars().count()
                ));
            }
            // Reactions trail the message they answer after an arrow, oldest
            // first, skipped entirely when there are none — so ordinary output is
            // unchanged. An arrow rather than brackets, because brackets collided
            // with the emoji: a double-width glyph overdraws `[` and `]` in real
            // terminals, and the arrow needs nothing on the far side. `--who`
            // names each sender beside its symbol, "me" for my own, the same
            // name-then-handle precedence every sender line already uses.
            let reactions = if trail == Trail::Off || message.tapbacks.is_empty() {
                String::new()
            } else if trail == Trail::Named {
                let named: Vec<String> = message
                    .tapbacks
                    .iter()
                    .map(|tapback| {
                        let sender = if tapback.is_from_me {
                            "me"
                        } else {
                            tapback
                                .contact_name
                                .as_deref()
                                .or(tapback.handle.as_deref())
                                .unwrap_or("unknown")
                        };
                        format!("{}{} {sender}", tapback.symbol, spill(&tapback.symbol))
                    })
                    .collect();
                format!(" ← {}", named.join(", "))
            } else {
                let symbols: String = message
                    .tapbacks
                    .iter()
                    .map(|tapback| format!("{}{}", tapback.symbol, spill(&tapback.symbol)))
                    .collect::<String>();
                format!(" ← {}", symbols.trim_end())
            };
            out.push_str(&format!(
                "{gutter}{stamp}  {where_}{}: {body}{reactions}\n",
                message.sender
            ));
        }
        out
    }
}

pub fn to_json<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(text) => format!("{text}\n"),
        Err(error) => format!("could not render JSON: {error}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ReplyTo, Tapback};

    fn chat(name: &str) -> Chat {
        Chat {
            rowid: 1,
            guid: "iMessage;+;c".into(),
            identifier: "c".into(),
            display_name: Some(name.into()),
            handles: None,
            named_handles: None,
            is_filtered: false,
            member_count: 1,
            is_group: false,
            last_date: Some(Utc::now()),
            message_count: 1,
            name: name.into(),
        }
    }

    fn at(iso: &str) -> Option<DateTime<Utc>> {
        Some(
            DateTime::parse_from_rfc3339(iso)
                .unwrap()
                .with_timezone(&Utc),
        )
    }

    #[test]
    fn a_missing_date_renders_as_nothing() {
        assert_eq!(format_timestamp(None), "");
        assert_eq!(relative_age(None), "never");
    }

    #[test]
    fn ages_read_in_the_largest_unit_that_fits() {
        let now = Utc::now();
        assert_eq!(relative_age(Some(now)), "just now");
        assert_eq!(
            relative_age(Some(now - chrono::Duration::seconds(59))),
            "just now"
        );
        assert_eq!(
            relative_age(Some(now - chrono::Duration::minutes(5))),
            "5m ago"
        );
        assert_eq!(
            relative_age(Some(now - chrono::Duration::hours(3))),
            "3h ago"
        );
        assert_eq!(
            relative_age(Some(now - chrono::Duration::days(9))),
            "9d ago"
        );
        // Past thirty days it becomes a date rather than a count.
        assert!(relative_age(Some(now - chrono::Duration::days(400))).contains(','));
    }

    /// A clock skewed ahead would otherwise produce "-3m ago".
    #[test]
    fn a_date_in_the_future_reads_as_just_now() {
        assert_eq!(
            relative_age(Some(Utc::now() + chrono::Duration::hours(2))),
            "just now"
        );
    }

    #[test]
    fn a_timestamp_has_no_leading_zero_on_the_hour() {
        // Rendered in local time, so assert the shape rather than the value.
        let stamp = format_timestamp(at("2026-01-15T17:30:00Z"));
        assert!(!stamp.starts_with('0'), "{stamp}");
        assert!(stamp.contains(':'), "{stamp}");
        assert!(stamp.ends_with("AM") || stamp.ends_with("PM"), "{stamp}");
    }

    #[test]
    fn truncation_counts_display_cells_rather_than_bytes_or_scalars() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        // Would have panicked on a byte boundary, and mis-measured before that.
        assert_eq!(truncate("café room", 6), "café …");
        assert_eq!(width_of(&pad_end("café", 8)), 8);
    }

    /// An emoji is one scalar value and two terminal cells. Counting scalars
    /// pushed every column after an emoji-named conversation one cell right,
    /// which is what the JavaScript build's UTF-16 count had accidentally got
    /// right and this had to be taught.
    #[test]
    fn an_emoji_is_two_cells_wide() {
        assert_eq!(width_of("😀"), 2);
        assert_eq!(width_of("😀😀"), 4);
        assert_eq!(width_of("AAAA"), 4);
        // Padding them to the same width must produce the same cell count.
        assert_eq!(width_of(&pad_end("😀😀", 9)), width_of(&pad_end("AAAA", 9)));
    }

    /// Every row of a rendered table has to occupy the same number of cells, or
    /// the columns after the widest name do not line up.
    #[test]
    fn every_rendered_row_is_the_same_width() {
        let names = ["AAAA", "😀😀", "Ship Room", "café", "日本語のグループ"];
        let chats: Vec<Chat> = names.iter().map(|name| chat(name)).collect();
        let rendered = render_chats(&chats);
        let widths: Vec<usize> = rendered
            .lines()
            .map(|line| width_of(line.trim_end()) + line.len() - line.trim_end().len())
            .collect();
        let full: Vec<usize> = rendered.lines().map(width_of).collect();
        assert!(
            full.windows(2).all(|pair| pair[0] == pair[1]),
            "rows differ in width: {full:?} (trimmed {widths:?})\n{rendered}"
        );
    }

    /// Truncation must not overrun its budget by splitting a wide character.
    #[test]
    fn truncation_never_overruns_its_budget() {
        for name in [
            "😀😀😀😀😀",
            "日本語のグループチャット",
            "aaaaaaaaaa",
            "a😀a😀a😀",
        ] {
            for width in 2..12 {
                let cut = truncate(name, width);
                assert!(
                    width_of(&cut) <= width,
                    "{name:?} at {width}: {cut:?} is {} cells",
                    width_of(&cut)
                );
            }
        }
    }

    #[test]
    fn an_empty_result_says_so_rather_than_printing_nothing() {
        assert_eq!(render_chats(&[]), "no chats found\n");
        assert_eq!(
            render_messages(
                &[],
                Render {
                    show_chat: false,
                    trail: Trail::Symbols,
                    day_headers: false,
                    styled: false
                }
            ),
            "no messages found\n"
        );
    }

    fn message(rowid: i64, sender: &str, body: &str) -> Message {
        Message {
            rowid,
            guid: format!("m{rowid}"),
            is_from_me: sender == "me",
            body: Some(body.into()),
            associated_message_type: 0,
            is_tapback: false,
            date: at("2026-01-15T17:30:00Z"),
            handle: None,
            contact_name: None,
            sender: sender.into(),
            chat_id: 1,
            chat_name: None,
            service: Some("iMessage".into()),
            attachments: Vec::new(),
            reply_to: None,
            tapbacks: Vec::new(),
            matched: true,
            group: None,
        }
    }

    /// A hit is marked, its context is not, and the two line up.
    ///
    /// Without a marker the output stops being answerable: you can read the
    /// conversation but not tell which line the search actually found.
    #[test]
    fn context_marks_the_hit_and_separates_the_runs() {
        let context = |rowid, group, body| {
            let mut message = message(rowid, "Dana Reyes", body);
            message.matched = false;
            message.group = Some(group);
            message
        };
        let hit = |rowid, group, body| {
            let mut message = message(rowid, "Dana Reyes", body);
            message.group = Some(group);
            message
        };

        let rendered = render_messages(
            &[
                context(1, 0, "before"),
                hit(2, 0, "the needle"),
                context(3, 1, "elsewhere"),
                hit(4, 1, "the needle again"),
            ],
            Render {
                show_chat: false,
                trail: Trail::Symbols,
                day_headers: false,
                styled: false,
            },
        );

        let lines: Vec<&str> = rendered.lines().collect();
        assert!(lines[0].starts_with("  Jan 15,"), "{lines:?}");
        assert!(lines[1].starts_with("> Jan 15,"), "{lines:?}");
        // A gap between runs, said the way grep says it.
        assert_eq!(lines[2], "--", "{lines:?}");
        assert!(lines[3].starts_with("  Jan 15,"), "{lines:?}");
        assert!(lines[4].starts_with("> Jan 15,"), "{lines:?}");
        // Only between runs, never before the first or after the last.
        assert_eq!(rendered.matches("\n--\n").count(), 1, "{rendered}");
    }

    /// Every search that asks for no context prints exactly what it always did.
    #[test]
    fn without_context_nothing_gains_a_gutter() {
        let rendered = render_messages(
            &[message(1, "Dana Reyes", "hello")],
            Render {
                show_chat: false,
                trail: Trail::Symbols,
                day_headers: false,
                styled: false,
            },
        );
        assert_eq!(rendered, "Jan 15, 9:30 AM  Dana Reyes: hello\n");
    }

    /// A reply names what it answers, above itself and indented to the width of
    /// a timestamp, so the transcript stays chronological and still reads.
    #[test]
    fn a_reply_says_what_it_is_answering() {
        let mut reply = message(2, "me", "yes, that works");
        reply.reply_to = Some(ReplyTo {
            rowid: 1,
            sender: "Dana Reyes".into(),
            excerpt: Some("are you around later".into()),
        });
        let out = render_messages(
            &[message(1, "Dana Reyes", "are you around later"), reply],
            Render {
                show_chat: false,
                trail: Trail::Symbols,
                day_headers: false,
                styled: false,
            },
        );

        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(
            lines[1].contains("↳ replying to Dana Reyes: are you around later"),
            "{out}"
        );
        // Indented to where the sender starts, not to column zero.
        assert!(lines[1].starts_with("       "), "{out}");
        assert!(lines[2].contains("me: yes, that works"), "{out}");
    }

    /// Every branch of the header, against one fixed clock — relative words
    /// while they are unambiguous, weekday inside the week, then the full
    /// date, the year only when foreign. 2026-07-10 is a Friday. Built in
    /// local civil time, so the same calendar day means the same header in
    /// every timezone the suite runs in.
    #[test]
    fn a_header_reads_relative_then_weekday_then_the_date() {
        use chrono::TimeZone;
        let civil = |y: i32, m: u32, d: u32| Local.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap();
        let now = civil(2026, 7, 10);
        let header = |y, m, d| day_header(civil(y, m, d), now);
        assert_eq!(header(2026, 7, 10), "Today");
        assert_eq!(header(2026, 7, 9), "Yesterday");
        assert_eq!(header(2026, 7, 7), "Tuesday");
        assert_eq!(header(2026, 7, 4), "Saturday");
        // Seven days back is a week ago today: the bare weekday would be
        // ambiguous with three days ago, so the full date takes over.
        assert_eq!(header(2026, 7, 3), "Friday, July 3");
        assert_eq!(header(2026, 6, 30), "Tuesday, June 30");
        assert_eq!(header(2025, 12, 31), "Wednesday, December 31, 2025");
    }

    /// A body's newlines fold to a visible mark, so one message is one
    /// transcript line however it was typed: `How are you doing?↵↵I'm fine.`
    /// Gray wraps only the mark when styled, `\r\n` folds once, and a
    /// trailing newline run is trimmed rather than drawn.
    #[test]
    fn a_newline_in_a_body_folds_to_a_visible_mark() {
        assert_eq!(
            one_line("How are you doing?\n\nI'm fine.", false),
            "How are you doing?↵↵I'm fine."
        );
        assert_eq!(one_line("a\r\nb", false), "a↵b", "\\r\\n folds once");
        assert_eq!(
            one_line("ends here\n\n", false),
            "ends here",
            "trailing trimmed"
        );
        let styled = one_line("a\nb", true);
        assert_eq!(styled, "a\x1b[90m↵\x1b[0mb");
        assert!(
            !styled.starts_with('\x1b') && !styled.ends_with('m'),
            "{styled:?}"
        );
    }

    /// Someone looking is necessary and not sufficient: NO_COLOR set and
    /// non-empty refuses the bold, TERM=dumb refuses it the older way, and a
    /// pipe never gets it whatever the environment says.
    #[test]
    fn styling_needs_a_terminal_and_no_refusal() {
        use std::ffi::OsStr;
        assert!(styling_allowed(None, None, true));
        assert!(!styling_allowed(None, None, false), "a pipe");
        assert!(
            !styling_allowed(Some(OsStr::new("1")), None, true),
            "NO_COLOR"
        );
        assert!(
            styling_allowed(Some(OsStr::new("")), None, true),
            "empty NO_COLOR does not count, per no-color.org"
        );
        assert!(
            !styling_allowed(None, Some(OsStr::new("dumb")), true),
            "TERM=dumb"
        );
        assert!(styling_allowed(
            None,
            Some(OsStr::new("xterm-256color")),
            true
        ));
    }

    /// A date header when the day changes and a bare time on every line —
    /// and the day survives across calls, which is the stream's whole case:
    /// one message per render, the header still lands exactly at the change.
    #[test]
    fn a_day_change_gets_a_header_and_messages_keep_only_a_time() {
        // Mid-June of the current local year, so the dates sit in this year
        // for any timezone and the no-year branch is the one under test — a
        // fixed 2026 here would start failing on the next New Year's Day.
        let year = Local::now().year();
        let mut first = message(1, "Dana Reyes", "late one");
        first.date = at(&format!("{year}-06-15T17:30:00Z"));
        let mut second = message(2, "me", "early the next");
        second.date = at(&format!("{year}-06-16T17:30:00Z"));

        let options = Render {
            show_chat: false,
            trail: Trail::Symbols,
            day_headers: true,
            styled: false,
        };
        let out = render_messages(&[first.clone(), second.clone()], options);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "{out}");
        // The header text itself is day_header's, pinned exhaustively in its
        // own test — here the contract is that the renderer emits exactly
        // that, once per day, wherever in the calendar the suite runs.
        let now = Local::now();
        assert_eq!(
            lines[0],
            day_header(first.date.unwrap().with_timezone(&Local), now),
            "{out}"
        );
        assert_eq!(
            lines[2],
            day_header(second.date.unwrap().with_timezone(&Local), now),
            "{out}"
        );
        assert_ne!(lines[0], lines[2], "{out}");
        // Message lines carry a time and never a date.
        assert!(
            lines[1].contains("late one") && !lines[1].contains("Jan"),
            "{out}"
        );

        // Bold is opt-in and wraps exactly the header: the escapes never
        // touch a message line, so a pipe that got bold by mistake would
        // still grep its messages — but it must not get it by mistake, which
        // the CLI test pins by asserting piped output is escape-free.
        let bold = render_messages(
            &[first.clone()],
            Render {
                styled: true,
                ..options
            },
        );
        let bold_lines: Vec<&str> = bold.lines().collect();
        assert!(
            bold_lines[0].starts_with("\x1b[1m") && bold_lines[0].ends_with("\x1b[0m"),
            "{bold:?}"
        );
        assert!(!bold_lines[1].contains('\x1b'), "{bold:?}");

        // The stream case: one message per call, same day state throughout.
        let mut renderer = Renderer::new(options);
        let one = renderer.render(std::slice::from_ref(&first));
        let two = renderer.render(std::slice::from_ref(&second));
        assert_eq!(one.lines().count(), 2, "{one}");
        let mut third = message(3, "me", "still the sixteenth");
        third.date = at(&format!("{year}-06-16T18:00:00Z"));
        let three = renderer.render(std::slice::from_ref(&third));
        assert_eq!(two.lines().count(), 2, "{two}");
        // Same day as the last emission: no header, the state remembered it.
        assert_eq!(three.lines().count(), 1, "{three}");
    }

    /// `--who` names each reaction's sender beside its symbol — "me" for my
    /// own, then the same contact-name-then-handle precedence every sender
    /// line uses — and without it the trail stays symbols alone.
    #[test]
    fn the_trail_names_who_reacted_only_when_asked() {
        let mut reacted = message(1, "Dana Reyes", "deploy is green");
        reacted.tapbacks = vec![
            Tapback {
                associated_message_type: 2000,
                symbol: "❤️".into(),
                date: at("2026-01-15T17:31:00Z"),
                is_from_me: false,
                handle: Some("+13105551234".into()),
                contact_name: Some("Sam Oyelaran".into()),
            },
            Tapback {
                associated_message_type: 2001,
                symbol: "👍".into(),
                date: at("2026-01-15T17:32:00Z"),
                is_from_me: true,
                handle: None,
                contact_name: None,
            },
        ];
        let named = render_messages(
            std::slice::from_ref(&reacted),
            Render {
                show_chat: false,
                trail: Trail::Named,
                day_headers: false,
                styled: false,
            },
        );
        assert!(named.contains("green ← ❤️  Sam Oyelaran, 👍 me"), "{named}");
        let bare = render_messages(
            std::slice::from_ref(&reacted),
            Render {
                show_chat: false,
                trail: Trail::Symbols,
                day_headers: false,
                styled: false,
            },
        );
        assert!(bare.contains("green ← ❤️ 👍"), "{bare}");
        assert!(!bare.contains("Sam"), "{bare}");
    }

    /// An ordinary message gains nothing, so a transcript without replies reads
    /// exactly as it did.
    #[test]
    fn a_message_that_is_not_a_reply_is_unchanged() {
        let out = render_messages(
            &[message(1, "Dana Reyes", "hello")],
            Render {
                show_chat: false,
                trail: Trail::Symbols,
                day_headers: false,
                styled: false,
            },
        );
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(!out.contains("↳"), "{out}");
    }

    #[test]
    fn a_reply_to_something_with_no_text_still_renders() {
        let mut reply = message(2, "me", "noted");
        reply.reply_to = Some(ReplyTo {
            rowid: 1,
            sender: "Dana Reyes".into(),
            excerpt: None,
        });
        let out = render_messages(
            &[reply],
            Render {
                show_chat: false,
                trail: Trail::Symbols,
                day_headers: false,
                styled: false,
            },
        );
        assert!(out.contains("↳ replying to Dana Reyes: (no text)"), "{out}");
    }
}
