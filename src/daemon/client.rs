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
        ErrorCode::SendDisabled => Error::SendDisabled(message),
        ErrorCode::Error | ErrorCode::Version => Error::Other(message),
    }
}

/// Send one request and return its result, closing the connection after.
pub fn request(mut stream: UnixStream, message: &Request) -> Result<serde_json::Value> {
    ask(&mut stream, message)?;
    let reader = BufReader::new(stream.try_clone()?);
    for line in reader.lines() {
        let line = line?;
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
