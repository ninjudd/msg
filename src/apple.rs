//! Apple-specific encodings used by the Messages database.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

/// Seconds between the Unix epoch and Apple's 2001-01-01 epoch.
const APPLE_EPOCH_OFFSET_SECONDS: i64 = 978_307_200;
const APPLE_EPOCH_OFFSET_MS: i64 = APPLE_EPOCH_OFFSET_SECONDS * 1_000;

/// Dates recorded before the 2011 schema change are in seconds; everything
/// since is in nanoseconds. Values above this are nanoseconds.
const NANOSECOND_THRESHOLD: u64 = 1_000_000_000_000;

const NANOSECONDS_PER_MS: i64 = 1_000_000;

/// Convert an Apple timestamp to a UTC instant.
///
/// Every arm saturates or returns `None` rather than wrapping: these values
/// arrive from a database this program does not write, and a row with a
/// nonsense date should read as undated rather than as a date in 1904.
pub fn from_apple_date(value: Option<i64>) -> Option<DateTime<Utc>> {
    let value = value?;
    if value == 0 {
        return None;
    }
    let ms = if value.unsigned_abs() > NANOSECOND_THRESHOLD {
        value / NANOSECONDS_PER_MS
    } else {
        value.checked_mul(1_000)?
    };
    DateTime::from_timestamp_millis(ms.checked_add(APPLE_EPOCH_OFFSET_MS)?)
}

/// Milliseconds since the Unix epoch to Apple's nanosecond timestamp.
///
/// Checked, because the range is narrower than it looks: `i64` nanoseconds
/// spans only ±292 years around 2001, so a date a user can easily type —
/// `1700-01-01`, `9999-01-01` — does not fit. Unchecked, this panicked in a
/// debug build and silently wrapped in a release one, where it became an
/// unrelated cutoff that returned the wrong messages with no error at all.
fn millis_to_apple(millis: i64) -> Option<i64> {
    millis
        .checked_sub(APPLE_EPOCH_OFFSET_MS)?
        .checked_mul(NANOSECONDS_PER_MS)
}

/// Convert an instant to Apple's nanosecond timestamp, or `None` if it does not
/// fit in one.
pub fn to_apple_date(date: DateTime<Utc>) -> Option<i64> {
    millis_to_apple(date.timestamp_millis())
}

/// A `--since` value this cannot turn into a cutoff.
#[derive(Debug)]
pub enum ParseTimeError {
    /// Neither a duration nor a date.
    Unparsed(String),
    /// A real date, outside what an Apple timestamp can hold.
    OutOfRange(String),
}

impl std::fmt::Display for ParseTimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unparsed(spec) => write!(
                f,
                "cannot parse time {spec}, expected something like 2h, 7d, or 2026-08-01"
            ),
            Self::OutOfRange(spec) => write!(
                f,
                "{spec} is outside the range a Messages timestamp can hold, \
                 which is roughly the years 1709 to 2293"
            ),
        }
    }
}

impl std::error::Error for ParseTimeError {}

/// Whether a string is `\d+(\.\d+)?` — the shape the duration pattern accepted.
///
/// Not `f64::from_str`, which also takes `-1`, `1e3`, `.5`, `inf` and `NaN`.
fn is_decimal(value: &str) -> bool {
    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if parts.next().is_some() {
        return false;
    }
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    match fraction {
        None => true,
        Some(digits) => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
    }
}

/// Parse `2h` or `7d` into milliseconds before now.
///
/// Saturating rather than wrapping: `99999999w` is a silly thing to type but it
/// should read as "everything", not as a cutoff in the far future.
fn parse_duration(spec: &str) -> Option<i64> {
    let unit = spec.chars().next_back()?;
    let scale_ms: i64 = match unit {
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        'w' => 604_800_000,
        _ => return None,
    };
    let amount = &spec[..spec.len() - unit.len_utf8()];
    if !is_decimal(amount) {
        return None;
    }
    let scaled = amount.parse::<f64>().ok()? * scale_ms as f64;
    if !scaled.is_finite() {
        return None;
    }
    // `as i64` saturates at the bounds, and so does the subtraction.
    Some(Utc::now().timestamp_millis().saturating_sub(scaled as i64))
}

/// Parse the date formats the TypeScript build accepted through `new Date`.
///
/// A bare `2026-01-15` is midnight **UTC**, not local. That is what JavaScript
/// does with a date-only ISO string, and the difference is a few hours of
/// messages at the boundary, so it is kept rather than quietly corrected.
fn parse_date(spec: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(spec) {
        return Some(parsed.with_timezone(&Utc));
    }
    for layout in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(spec, layout) {
            // A time written without an offset is the one the user's clock
            // shows. Ambiguous across a DST fall-back, where the earlier of the
            // two readings is the one that reaches further back.
            return Local
                .from_local_datetime(&naive)
                .earliest()
                .map(|local| local.with_timezone(&Utc));
        }
    }
    NaiveDate::parse_from_str(spec, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| Utc.from_utc_datetime(&naive))
}

/// Parse a duration like `2h` or `7d`, or a date, into an Apple timestamp.
pub fn since_to_apple_date(spec: &str) -> Result<i64, ParseTimeError> {
    let trimmed = spec.trim();
    let out_of_range = || ParseTimeError::OutOfRange(spec.to_string());
    if let Some(millis) = parse_duration(trimmed) {
        return millis_to_apple(millis).ok_or_else(out_of_range);
    }
    let date = parse_date(trimmed).ok_or_else(|| ParseTimeError::Unparsed(spec.to_string()))?;
    to_apple_date(date).ok_or_else(out_of_range)
}

/// Marker bytes that open an NSArchiver typedstream.
const STREAM_HEADER: &[u8] = b"\x04\x0bstreamtyped";

/// Type marker introducing a C string in a typedstream.
const STRING_MARKER: u8 = 0x2b;

const INT_16: u8 = 0x81;
const INT_32: u8 = 0x82;
const INT_64: u8 = 0x83;

struct ReadInt {
    value: i64,
    next: usize,
}

/// Read a typedstream integer.
///
/// Values are a single **signed** byte unless escaped by a width marker, so
/// anything from 0x80 up that is not a marker reads negative — which the caller
/// rejects, and should.
fn read_int(data: &[u8], offset: usize) -> Option<ReadInt> {
    let marker = *data.get(offset)?;
    let width = match marker {
        INT_16 => 2,
        INT_32 => 4,
        INT_64 => 8,
        _ => {
            return Some(ReadInt {
                value: i64::from(marker as i8),
                next: offset + 1,
            });
        }
    };
    let start = offset + 1;
    let bytes = data.get(start..start + width)?;
    let mut buffer = [0u8; 8];
    buffer[..width].copy_from_slice(bytes);
    Some(ReadInt {
        // Saturating rather than wrapping: an eight-byte length above
        // `i64::MAX` is nonsense either way, and the caller's bounds check is
        // what turns it into `None`.
        value: i64::try_from(u64::from_le_bytes(buffer)).unwrap_or(i64::MAX),
        next: start + width,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Extract the message text from an archived NSAttributedString.
///
/// Modern macOS leaves `message.text` NULL and stores the body here. The string
/// contents follow the NSString class name and a 0x2b type marker.
pub fn decode_attributed_body(blob: Option<&[u8]>) -> Option<String> {
    let data = blob?;
    if !data.starts_with(STREAM_HEADER) {
        return None;
    }

    let start = find(data, b"NSString").or_else(|| find(data, b"NSMutableString"))?;
    let marker = start
        + data[start..]
            .iter()
            .position(|&byte| byte == STRING_MARKER)?;

    let length = read_int(data, marker + 1)?;
    if length.value <= 0 {
        return None;
    }
    let end = length
        .next
        .checked_add(usize::try_from(length.value).ok()?)?;
    let bytes = data.get(length.next..end)?;
    // Lossy, like Buffer.toString('utf8'): a body that is nearly all readable
    // is worth more than nothing at all.
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Return a message's text, falling back to the archived body.
pub fn message_body(text: Option<String>, attributed_body: Option<&[u8]>) -> Option<String> {
    text.or_else(|| decode_attributed_body(attributed_body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a typedstream blob shaped like the one Messages stores.
    fn typed_stream(text: &str) -> Vec<u8> {
        let body = text.as_bytes();
        let mut blob = Vec::new();
        blob.extend_from_slice(STREAM_HEADER);
        blob.extend_from_slice(&[0x81, 0xe8, 0x03, 0x84, 0x01, 0x40, 0x84, 0x84, 0x84]);
        blob.extend_from_slice(b"NSString");
        blob.extend_from_slice(&[0x01, 0x94, 0x84, 0x01, 0x2b]);
        if body.len() < 0x80 {
            blob.push(body.len() as u8);
        } else {
            blob.push(INT_16);
            blob.extend_from_slice(&(body.len() as u16).to_le_bytes());
        }
        blob.extend_from_slice(body);
        blob
    }

    fn iso(date: Option<DateTime<Utc>>) -> String {
        date.expect("a date")
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    #[test]
    fn reads_nanosecond_timestamps() {
        // 2026-08-06T00:00:00Z is 807_667_200 seconds after the Apple epoch.
        let date = from_apple_date(Some(807_667_200 * 1_000_000_000));
        assert_eq!(iso(date), "2026-08-06T00:00:00.000Z");
    }

    #[test]
    fn reads_legacy_second_timestamps() {
        assert_eq!(
            iso(from_apple_date(Some(807_667_200))),
            "2026-08-06T00:00:00.000Z"
        );
    }

    #[test]
    fn survives_values_beyond_the_javascript_safe_integer() {
        // The value that forced BigInt arithmetic in the TypeScript build. In
        // Rust it is an ordinary i64, which is the point.
        let raw = 807_667_200i64 * 1_000_000_000;
        assert!(raw > 9_007_199_254_740_991);
        assert!(from_apple_date(Some(raw)).is_some());
    }

    #[test]
    fn round_trips_through_to_apple_date() {
        let original = DateTime::parse_from_rfc3339("2026-03-01T12:34:56Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(from_apple_date(to_apple_date(original)), Some(original));
    }

    #[test]
    fn treats_null_and_zero_as_no_date() {
        assert_eq!(from_apple_date(None), None);
        assert_eq!(from_apple_date(Some(0)), None);
    }

    #[test]
    fn accepts_durations() {
        let two_hours = from_apple_date(Some(since_to_apple_date("2h").unwrap())).unwrap();
        let expected = Utc::now().timestamp_millis() - 2 * 3_600_000;
        assert!((two_hours.timestamp_millis() - expected).abs() < 5_000);
    }

    #[test]
    fn accepts_fractional_durations() {
        let ninety = from_apple_date(Some(since_to_apple_date("1.5h").unwrap())).unwrap();
        let expected = Utc::now().timestamp_millis() - 90 * 60_000;
        assert!((ninety.timestamp_millis() - expected).abs() < 5_000);
    }

    #[test]
    fn accepts_iso_dates() {
        let date = from_apple_date(Some(since_to_apple_date("2026-01-15").unwrap()));
        assert_eq!(iso(date), "2026-01-15T00:00:00.000Z");
    }

    #[test]
    fn accepts_a_full_timestamp() {
        let date = from_apple_date(Some(since_to_apple_date("2026-01-15T17:30:00Z").unwrap()));
        assert_eq!(iso(date), "2026-01-15T17:30:00.000Z");
    }

    #[test]
    fn rejects_nonsense() {
        for spec in [
            "soonish",
            "",
            "-1h",
            "1e3h",
            ".5h",
            "1.h",
            "h",
            "2026-13-45",
            "2y",
        ] {
            assert!(since_to_apple_date(spec).is_err(), "accepted {spec:?}");
        }
    }

    /// `i64` nanoseconds reach only about 292 years either side of 2001, so
    /// dates a user can easily type do not fit. Unchecked this panicked in a
    /// debug build and, worse, wrapped silently in a release one into an
    /// unrelated cutoff that returned the wrong messages with no error at all.
    #[test]
    fn refuses_a_date_outside_what_an_apple_timestamp_can_hold() {
        for spec in ["9999-01-01", "1700-01-01", "1000-01-01", "2500-06-01"] {
            let error = since_to_apple_date(spec).unwrap_err();
            assert!(
                matches!(error, ParseTimeError::OutOfRange(_)),
                "{spec} gave {error:?}"
            );
            assert!(error.to_string().contains("outside the range"), "{spec}");
        }
    }

    #[test]
    fn accepts_the_edges_of_what_does_fit() {
        for spec in ["1710-01-01", "2292-01-01", "2026-08-07"] {
            assert!(since_to_apple_date(spec).is_ok(), "{spec} was refused");
        }
    }

    /// A silly duration should read as "everything", not wrap into the future.
    #[test]
    fn an_enormous_duration_saturates_rather_than_wrapping() {
        let since = since_to_apple_date("99999999999w");
        assert!(
            since.is_err() || since.is_ok_and(|value| value < 0),
            "a huge duration produced a cutoff in the future"
        );
    }

    #[test]
    fn the_error_names_what_it_could_not_parse() {
        let error = since_to_apple_date("soonish").unwrap_err();
        assert!(matches!(error, ParseTimeError::Unparsed(_)), "{error:?}");
        let text = error.to_string();
        assert!(text.contains("cannot parse time"), "{text}");
        assert!(text.contains("soonish"), "{text}");
    }

    #[test]
    fn decodes_a_short_string() {
        assert_eq!(
            decode_attributed_body(Some(&typed_stream("hey are you around"))).as_deref(),
            Some("hey are you around")
        );
    }

    #[test]
    fn decodes_a_string_longer_than_one_length_byte() {
        let long = "x".repeat(500);
        assert_eq!(
            decode_attributed_body(Some(&typed_stream(&long))).as_deref(),
            Some(long.as_str())
        );
    }

    #[test]
    fn decodes_multi_byte_characters() {
        assert_eq!(
            decode_attributed_body(Some(&typed_stream("café ☕️"))).as_deref(),
            Some("café ☕️")
        );
    }

    #[test]
    fn returns_none_for_a_blob_that_is_not_a_typedstream() {
        assert_eq!(decode_attributed_body(Some(b"not an archive")), None);
    }

    #[test]
    fn returns_none_for_empty_input() {
        assert_eq!(decode_attributed_body(None), None);
        assert_eq!(decode_attributed_body(Some(&[])), None);
    }

    /// Every prefix of a real blob, which is what a truncated column looks like.
    /// None of them may panic, and none may return text that was not there.
    #[test]
    fn survives_a_truncated_blob() {
        let full = typed_stream("hey are you around");
        for length in 0..full.len() {
            let decoded = decode_attributed_body(Some(&full[..length]));
            assert!(
                decoded.is_none_or(|text| "hey are you around".starts_with(&text)),
                "invented text from a {length}-byte prefix"
            );
        }
    }

    /// A length field claiming more bytes than the blob holds must not read past
    /// the end. In TypeScript this was a bounds check; here it would be a panic.
    #[test]
    fn refuses_a_length_that_runs_past_the_end() {
        let mut blob = typed_stream("hello");
        let marker = blob
            .iter()
            .rposition(|&byte| byte == STRING_MARKER)
            .unwrap();
        blob[marker + 1] = 0x7f;
        assert_eq!(decode_attributed_body(Some(&blob)), None);
    }

    #[test]
    fn refuses_a_negative_length() {
        let mut blob = typed_stream("hello");
        let marker = blob
            .iter()
            .rposition(|&byte| byte == STRING_MARKER)
            .unwrap();
        // 0x84 is not a width marker, so it reads as the signed byte -124.
        blob[marker + 1] = 0x84;
        assert_eq!(decode_attributed_body(Some(&blob)), None);
    }

    #[test]
    fn message_body_prefers_the_text_column() {
        let archived = typed_stream("archived");
        assert_eq!(
            message_body(Some("plain text".into()), Some(&archived)).as_deref(),
            Some("plain text")
        );
    }

    #[test]
    fn message_body_falls_back_to_the_archived_body() {
        let archived = typed_stream("archived");
        assert_eq!(
            message_body(None, Some(&archived)).as_deref(),
            Some("archived")
        );
    }

    #[test]
    fn message_body_is_none_when_both_are_missing() {
        assert_eq!(message_body(None, None), None);
    }
}
