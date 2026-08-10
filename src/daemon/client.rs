//! The client half of the wire: connect, ask one question, read the answer.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::daemon::protocol::{
    ErrorCode, Frame, Request, SavePart, SaveReply, SaveRequest, WatchRequest, envelope,
    socket_path,
};
use crate::{Error, Result};

/// Long enough to cover a busy daemon, short enough not to look like a hang.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Connect to the daemon, or answer `None` when nothing is listening.
///
/// `None` is an ordinary outcome, not an error: the CLI falls back to reading
/// the database itself, which is the path a machine without the daemon installed
/// takes on every command.
pub fn connect_daemon(path: Option<&Path>) -> Option<UnixStream> {
    connect_daemon_within(path, RESPONSE_TIMEOUT)
}

/// Connect with a caller-chosen deadline instead of the general one.
///
/// A socket that accepts and then says nothing is indistinguishable from a busy
/// daemon until the read times out, so the wait is the only thing separating
/// them. Thirty seconds is the right wait for a command that has nothing else
/// to do; it is the wrong one for a caller that has a fallback and would rather
/// take it than sit there.
pub fn connect_daemon_within(path: Option<&Path>, timeout: Duration) -> Option<UnixStream> {
    let owned = path.map_or_else(socket_path, Path::to_path_buf);
    let stream = UnixStream::connect(owned).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    Some(stream)
}

fn ask(stream: &mut UnixStream, request: &Request) -> Result<()> {
    stream.write_all(envelope(request)?.as_bytes())?;
    Ok(())
}

fn decode(line: &str) -> Result<Frame> {
    serde_json::from_str(line).map_err(|_| {
        let head: String = line.chars().take(80).collect();
        Error::other(format!("unexpected frame from msgd: {head}"))
    })
}

fn raise(code: ErrorCode, message: String) -> Error {
    match code {
        ErrorCode::AccessDenied => Error::AccessDenied(message),
        ErrorCode::Ambiguous => Error::Ambiguous(message),
        ErrorCode::SendDisabled => Error::SendDisabled(message),
        ErrorCode::Error | ErrorCode::Version => Error::Other(message),
    }
}

/// The read deadline elapsing is the one io failure here a person can act on,
/// so it gets a sentence instead of an errno. Without this, thirty seconds of
/// silence ended as `Resource temporarily unavailable (os error 35)` — eleven
/// words naming no command, no daemon, and no timeout, indistinguishable from
/// a crash. Reachable since the word-boundary rule let a search outlive the
/// deadline (search-boundaries.md §6).
fn ran_out_of_time(request: &Request, error: std::io::Error) -> Error {
    use std::io::ErrorKind;
    if !matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
        return error.into();
    }
    let command = serde_json::to_value(request)
        .ok()
        .and_then(|value| value["cmd"].as_str().map(str::to_string))
        .unwrap_or_else(|| "the request".to_string());
    Error::other(format!(
        "msgd took more than {} seconds to answer `{command}`, so msg stopped waiting. \
         That is a timeout, not a crash — the daemon is likely still working through it. \
         `msg daemon status` shows whether it is up; a narrower query answers sooner.",
        RESPONSE_TIMEOUT.as_secs()
    ))
}

/// Send one request and return its result, closing the connection after.
pub fn request(mut stream: UnixStream, message: &Request) -> Result<serde_json::Value> {
    ask(&mut stream, message)?;
    let reader = BufReader::new(stream.try_clone()?);
    for line in reader.lines() {
        let line = line.map_err(|error| ran_out_of_time(message, error))?;
        if line.trim().is_empty() {
            continue;
        }
        match decode(line.trim())? {
            Frame::Error { code, message } => return Err(raise(code, message)),
            Frame::Result { value } => return Ok(value),
            Frame::Item { .. } => continue,
        }
    }
    Err(Error::other("msgd closed the connection without answering"))
}

/// Send a watch request and hand each message to `on_message` as it arrives,
/// until the callback asks to stop or the daemon goes away.
///
/// Returns `Ok(())` only when the caller stopped it. Reaching the end of the
/// stream means the daemon went away — stopped, upgraded, or crashed — which is
/// an error, because `watch` runs until interrupted and returning quietly would
/// have a pipeline or supervisor believe it is still following.
/// Read one attachment's chunks, handing each to `on_part` as it lands.
///
/// Unlike `watch`, this ends: the final `result` frame is the answer, and the
/// caller uses it to know the transfer finished rather than was cut off. A
/// stream that stops without one has to be treated as a failure, or a truncated
/// file would be indistinguishable from a complete one.
pub fn save(
    mut stream: UnixStream,
    message: &SaveRequest,
    mut on_part: impl FnMut(SavePart) -> Result<()>,
) -> Result<SaveReply> {
    // A large attachment takes longer than a query, and the client is doing
    // nothing but writing what arrives.
    stream.set_read_timeout(None)?;
    ask(&mut stream, &Request::Save(message.clone()))?;

    let reader = BufReader::new(stream.try_clone()?);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match decode(line.trim())? {
            Frame::Error { code, message } => return Err(raise(code, message)),
            Frame::Item { value } => on_part(serde_json::from_value(value)?)?,
            Frame::Result { value } => return Ok(serde_json::from_value(value)?),
        }
    }
    Err(Error::other(
        "msgd stopped part-way through the attachment, so it was not saved whole",
    ))
}

pub fn watch(
    mut stream: UnixStream,
    message: &WatchRequest,
    mut on_message: impl FnMut(serde_json::Value) -> Result<()>,
) -> Result<()> {
    // A watch has no deadline; the whole point is to wait.
    stream.set_read_timeout(None)?;
    ask(&mut stream, &Request::Watch(message.clone()))?;

    let reader = BufReader::new(stream.try_clone()?);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match decode(line.trim())? {
            Frame::Error { code, message } => return Err(raise(code, message)),
            Frame::Item { value } => on_message(value)?,
            Frame::Result { .. } => {}
        }
    }
    Err(Error::other(
        "msgd stopped, so watch ended. Check `msg daemon status`.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::SearchRequest;

    /// Thirty seconds of silence used to end as `Resource temporarily
    /// unavailable (os error 35)` — nothing named the command, the daemon, or
    /// the deadline, so a timeout read as a crash.
    #[test]
    fn a_timeout_answers_with_words_rather_than_an_errno() {
        let request = Request::Search(SearchRequest {
            query: "ing".into(),
            ..Default::default()
        });
        let timeout = std::io::Error::from(std::io::ErrorKind::WouldBlock);
        let words = ran_out_of_time(&request, timeout).to_string();
        assert!(words.contains("`search`"), "{words}");
        assert!(words.contains("timeout, not a crash"), "{words}");
        assert!(words.contains("msg daemon status"), "{words}");

        // Every other io failure keeps its own words — mapping them all to
        // "timeout" would misname a genuinely dead daemon.
        let other = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        let words = ran_out_of_time(&request, other).to_string();
        assert!(!words.contains("timeout"), "{words}");
    }
}
