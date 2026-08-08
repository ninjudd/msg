//! Rendering for terminal and JSON output.

use chrono::{DateTime, Datelike, Local, Utc};

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

/// Character count, not byte count: these widths line up columns on a terminal,
/// and `len()` would pad a name with an accent in it one column short.
fn width_of(value: &str) -> usize {
    value.chars().count()
}

fn truncate(value: &str, width: usize) -> String {
    if width_of(value) <= width {
        return value.to_string();
    }
    let mut out: String = value.chars().take(width.saturating_sub(1)).collect();
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

pub fn render_messages(messages: &[Message], show_chat: bool) -> String {
    if messages.is_empty() {
        return "no messages found\n".to_string();
    }
    let mut out = String::new();
    for message in messages {
        let stamp = format_timestamp(message.date);
        let where_ = match (show_chat, message.chat_name.as_deref()) {
            (true, Some(name)) => format!("[{name}] "),
            _ => String::new(),
        };
        let body = message.body.as_deref().unwrap_or("(no text)");
        out.push_str(&format!("{stamp}  {where_}{}: {body}\n", message.sender));
    }
    out
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
    fn truncation_counts_characters_rather_than_bytes() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        // Would have panicked on a byte boundary, and mis-measured before that.
        assert_eq!(truncate("café ☕️ room", 6), "café …");
        assert_eq!(width_of(&pad_end("café", 8)), 8);
    }

    #[test]
    fn an_empty_result_says_so_rather_than_printing_nothing() {
        assert_eq!(render_chats(&[]), "no chats found\n");
        assert_eq!(render_messages(&[], false), "no messages found\n");
    }
}
