//! Transport-neutral line framing and request multiplexing for byte-stream transports.
//!
//! The Windows named pipe runs both ends of this module; it is generic over any
//! `AsyncRead + AsyncWrite` so the same logic is tested over an in-memory duplex on every
//! platform (`docs/ipc.md` section 3).
//!
//! Server side ([`serve_connection`]): read the hello, answer the welcome, then serve
//! requests concurrently (up to [`MAX_IN_FLIGHT`] per connection) while forwarding events.
//! Client side ([`PipeClient`]): send the hello, check the welcome, then one driver task owns
//! the stream and matches responses to waiting callers by `id` while events interleave.
//!
//! Logging follows SR-32: request `op` names and error codes only, never message bodies.

// Only the Windows transport uses this module in production builds; the unit tests below
// exercise it on every platform.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

use crate::util::{AbortOnDrop, EVENT_CAPACITY};
use crate::wire::{ClientMessage, ServerMessage};
use crate::{ErrorCode, Event, IpcError, Ok, Request, RequestHandler, Response};
use crate::{IPC_VERSION, MAX_MESSAGE_BYTES};

/// Most requests one connection may have in flight at once. Past this the server stops
/// reading from that connection until an answer goes out, which is natural backpressure.
pub(crate) const MAX_IN_FLIGHT: usize = 64;

/// Correlation ID the daemon uses for an error about the connection itself (a refused
/// hello). Clients start their own IDs at 1.
const CONNECTION_ID: u64 = 0;

/// A newline-delimited JSON connection: reads are framed by a [`LinesCodec`] capped at
/// [`MAX_MESSAGE_BYTES`] per line; writes go out as one buffer per line and are flushed.
pub(crate) struct LineStream<T> {
    reader: FramedRead<ReadHalf<T>, LinesCodec>,
    writer: WriteHalf<T>,
}

impl<T> LineStream<T>
where
    T: AsyncRead + AsyncWrite,
{
    /// The next line, `None` at end of stream.
    async fn next(&mut self) -> Option<Result<String, LinesCodecError>> {
        self.reader.next().await
    }

    /// Write one line. `line` must not contain a newline (`serde_json` never emits one).
    async fn send(&mut self, line: String) -> std::io::Result<()> {
        let mut buf = line.into_bytes();
        buf.push(b'\n');
        self.writer.write_all(&buf).await?;
        self.writer.flush().await
    }
}

/// Wrap a byte stream in the line codec both pipe ends use.
pub(crate) fn framed<T>(io: T) -> LineStream<T>
where
    T: AsyncRead + AsyncWrite,
{
    let (read, writer) = tokio::io::split(io);
    LineStream {
        reader: FramedRead::new(read, LinesCodec::new_with_max_length(MAX_MESSAGE_BYTES)),
        writer,
    }
}

/// What went wrong on a pipe connection.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PipeError {
    /// A line exceeded [`MAX_MESSAGE_BYTES`]; the connection is dropped.
    #[error("message exceeds the {MAX_MESSAGE_BYTES} byte cap")]
    TooLong,
    /// The stream failed.
    #[error("pipe i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The other end hung up.
    #[error("the other end closed the connection")]
    Closed,
    /// The other end broke the protocol.
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    /// The two ends speak different contract versions.
    #[error("daemon IPC version {daemon} does not match client {client}")]
    Version {
        /// Daemon's version.
        daemon: u32,
        /// Client's version.
        client: u32,
    },
}

impl From<LinesCodecError> for PipeError {
    fn from(e: LinesCodecError) -> Self {
        match e {
            LinesCodecError::MaxLineLengthExceeded => Self::TooLong,
            LinesCodecError::Io(e) => Self::Io(e),
        }
    }
}

/// Serialise one message, refusing anything the other end's codec would reject.
fn encode<M: Serialize>(message: &M) -> Result<String, PipeError> {
    let line = serde_json::to_string(message)
        .map_err(|_| PipeError::Protocol("message did not serialise"))?;
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(PipeError::TooLong);
    }
    Result::Ok(line)
}

async fn send_message<T, M>(stream: &mut LineStream<T>, message: &M) -> Result<(), PipeError>
where
    T: AsyncRead + AsyncWrite,
    M: Serialize,
{
    let line = encode(message)?;
    stream.send(line).await?;
    Result::Ok(())
}

async fn next_line<T>(stream: &mut LineStream<T>) -> Result<String, PipeError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    match stream.next().await {
        Some(line) => Result::Ok(line?),
        None => Err(PipeError::Closed),
    }
}

// ------------------------------------------------------------------------------------------
// Server side
// ------------------------------------------------------------------------------------------

/// A request line whose `request` did not parse (unknown `op`, bad fields) but whose `id`
/// did, so the client still gets a matched `BadRequest` instead of a dropped connection.
#[derive(Deserialize)]
struct LooseRequest {
    #[serde(rename = "type")]
    kind: String,
    id: u64,
}

enum Incoming {
    Request { id: u64, request: Request },
    Unparsable { id: u64 },
    Hello,
}

fn parse_incoming(line: &str) -> Result<Incoming, PipeError> {
    match serde_json::from_str::<ClientMessage>(line) {
        Result::Ok(ClientMessage::Request { id, request }) => {
            Result::Ok(Incoming::Request { id, request })
        }
        Result::Ok(ClientMessage::Hello { .. }) => Result::Ok(Incoming::Hello),
        Err(_) => match serde_json::from_str::<LooseRequest>(line) {
            Result::Ok(loose) if loose.kind == "request" => {
                Result::Ok(Incoming::Unparsable { id: loose.id })
            }
            _ => Err(PipeError::Protocol("unparsable line")),
        },
    }
}

/// Answer a request, replacing an answer too large for the codec with a `TooLarge` error so
/// the client is never left waiting.
async fn send_response<T>(
    stream: &mut LineStream<T>,
    id: u64,
    response: Response,
) -> Result<(), PipeError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    match send_message(stream, &ServerMessage::Response { id, response }).await {
        Err(PipeError::TooLong) => {
            tracing::warn!(id, "ipc response over the size cap, answering too_large");
            let response = Response::err(
                ErrorCode::TooLarge,
                "the answer exceeds the IPC message size limit",
            );
            send_message(stream, &ServerMessage::Response { id, response }).await
        }
        other => other,
    }
}

/// Serve one authenticated pipe connection until the client hangs up, the daemon closes the
/// event channel, or the client breaks the protocol.
///
/// `events` must be subscribed when the connection is accepted, so the client sees every
/// event from the moment it connected. The caller has already run the SR-27 token check.
pub(crate) async fn serve_connection<T>(
    io: T,
    handler: Arc<dyn RequestHandler>,
    mut events: broadcast::Receiver<Event>,
    daemon_version: &str,
) -> Result<(), PipeError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = framed(io);

    let hello = next_line(&mut stream).await?;
    let Result::Ok(ClientMessage::Hello { ipc_version }) =
        serde_json::from_str::<ClientMessage>(&hello)
    else {
        return Err(PipeError::Protocol("the first line must be a hello"));
    };
    send_message(
        &mut stream,
        &ServerMessage::Welcome {
            ipc_version: IPC_VERSION,
            daemon_version: daemon_version.to_owned(),
        },
    )
    .await?;
    if ipc_version != IPC_VERSION {
        let response = Response::err(
            ErrorCode::BadRequest,
            format!("the daemon speaks IPC version {IPC_VERSION}, the client {ipc_version}"),
        );
        send_response(&mut stream, CONNECTION_ID, response).await?;
        return Err(PipeError::Version {
            daemon: IPC_VERSION,
            client: ipc_version,
        });
    }

    let mut in_flight: FuturesUnordered<BoxFuture<'static, (u64, Response)>> =
        FuturesUnordered::new();
    loop {
        tokio::select! {
            line = stream.next(), if in_flight.len() < MAX_IN_FLIGHT => {
                let Some(line) = line else { return Result::Ok(()) };
                match parse_incoming(&line?)? {
                    Incoming::Request { id, request } => {
                        tracing::debug!(op = request.op(), id, "ipc request");
                        let handler = Arc::clone(&handler);
                        in_flight.push(
                            async move {
                                let response = handler.handle(request).await;
                                (id, response)
                            }
                            .boxed(),
                        );
                    }
                    Incoming::Unparsable { id } => {
                        tracing::debug!(id, "ipc request did not parse");
                        let response =
                            Response::err(ErrorCode::BadRequest, "request did not parse");
                        send_response(&mut stream, id, response).await?;
                    }
                    Incoming::Hello => return Err(PipeError::Protocol("a second hello")),
                }
            }
            Some((id, response)) = in_flight.next(), if !in_flight.is_empty() => {
                if let Some(code) = response.error_code() {
                    tracing::debug!(id, %code, "ipc request failed");
                }
                send_response(&mut stream, id, response).await?;
            }
            event = events.recv() => match event {
                Result::Ok(event) => {
                    match send_message(&mut stream, &ServerMessage::Event { event }).await {
                        Err(PipeError::TooLong) => {
                            tracing::warn!("ipc event over the size cap, dropped");
                        }
                        other => other?,
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "ipc client fell behind, events dropped");
                }
                Err(broadcast::error::RecvError::Closed) => return Result::Ok(()),
            },
        }
    }
}

// ------------------------------------------------------------------------------------------
// Client side
// ------------------------------------------------------------------------------------------

struct Outgoing {
    id: u64,
    line: String,
    reply: oneshot::Sender<Response>,
}

/// The client end of a pipe connection after a successful hello.
///
/// Cheap to share: `request` takes `&self` and may be called concurrently; one driver task
/// owns the stream. Dropping the client stops the driver and closes the connection.
pub(crate) struct PipeClient {
    outgoing: mpsc::Sender<Outgoing>,
    events: broadcast::WeakSender<Event>,
    next_id: AtomicU64,
    daemon_version: String,
    _driver: AbortOnDrop,
}

impl PipeClient {
    /// Send the hello, wait for the welcome, check the version and start the driver.
    pub(crate) async fn handshake<T>(io: T) -> Result<Self, PipeError>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut stream = framed(io);
        send_message(
            &mut stream,
            &ClientMessage::Hello {
                ipc_version: IPC_VERSION,
            },
        )
        .await?;
        let welcome = next_line(&mut stream).await?;
        let Result::Ok(ServerMessage::Welcome {
            ipc_version,
            daemon_version,
        }) = serde_json::from_str::<ServerMessage>(&welcome)
        else {
            return Err(PipeError::Protocol(
                "the daemon did not answer with a welcome",
            ));
        };
        if ipc_version != IPC_VERSION {
            return Err(PipeError::Version {
                daemon: ipc_version,
                client: IPC_VERSION,
            });
        }

        let (outgoing, outgoing_rx) = mpsc::channel(MAX_IN_FLIGHT);
        let (events_tx, _) = broadcast::channel(EVENT_CAPACITY);
        let events = events_tx.downgrade();
        let driver = tokio::spawn(drive(stream, outgoing_rx, events_tx));
        Result::Ok(Self {
            outgoing,
            events,
            next_id: AtomicU64::new(CONNECTION_ID + 1),
            daemon_version,
            _driver: AbortOnDrop::new(driver),
        })
    }

    /// The daemon's package version from the welcome.
    pub(crate) fn daemon_version(&self) -> &str {
        &self.daemon_version
    }

    /// Send one request and wait for its answer. Transport failures become
    /// [`ErrorCode::Internal`]; an oversized request becomes [`ErrorCode::TooLarge`].
    pub(crate) async fn request(&self, request: &Request) -> Result<Ok, IpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = match encode(&ClientMessage::Request {
            id,
            request: request.clone(),
        }) {
            Result::Ok(line) => line,
            Err(PipeError::TooLong) => {
                return Err(IpcError::new(
                    ErrorCode::TooLarge,
                    "the request exceeds the IPC message size limit",
                ))
            }
            Err(e) => return Err(IpcError::new(ErrorCode::Internal, e.to_string())),
        };
        let (reply, answer) = oneshot::channel();
        let gone = || IpcError::new(ErrorCode::Internal, "the PlayDrop daemon went away");
        self.outgoing
            .send(Outgoing { id, line, reply })
            .await
            .map_err(|_| gone())?;
        let response = answer.await.map_err(|_| gone())?;
        response.into()
    }

    /// Events from now on. Ends when the connection closes.
    pub(crate) fn events(&self) -> BroadcastStream<Event> {
        events_stream(&self.events)
    }
}

/// Subscribe to a client's event channel, or return an already finished stream when the
/// connection is gone.
pub(crate) fn events_stream(events: &broadcast::WeakSender<Event>) -> BroadcastStream<Event> {
    let rx = if let Some(tx) = events.upgrade() {
        tx.subscribe()
    } else {
        let (tx, rx) = broadcast::channel(1);
        drop(tx);
        rx
    };
    BroadcastStream::new(rx)
}

/// Own the stream: write outgoing requests, route responses by `id`, publish events. Exits
/// when the client is dropped or the daemon hangs up; waiting callers then see "went away".
async fn drive<T>(
    mut stream: LineStream<T>,
    mut outgoing: mpsc::Receiver<Outgoing>,
    events: broadcast::Sender<Event>,
) where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut pending: HashMap<u64, oneshot::Sender<Response>> = HashMap::new();
    loop {
        tokio::select! {
            next = outgoing.recv() => {
                let Some(Outgoing { id, line, reply }) = next else { break };
                pending.insert(id, reply);
                if let Err(e) = stream.send(line).await {
                    tracing::warn!(error = %e, "ipc pipe write failed");
                    break;
                }
            }
            line = stream.next() => match line {
                None => break,
                Some(Err(e)) => {
                    tracing::warn!(error = %PipeError::from(e), "ipc pipe read failed");
                    break;
                }
                Some(Result::Ok(line)) => match serde_json::from_str::<ServerMessage>(&line) {
                    Result::Ok(ServerMessage::Response { id, response }) => {
                        if let Some(reply) = pending.remove(&id) {
                            // The caller may have stopped waiting; that is fine.
                            let _ = reply.send(response);
                        } else if id == CONNECTION_ID {
                            tracing::warn!(code = ?response.error_code(), "daemon refused the connection");
                            break;
                        }
                    }
                    Result::Ok(ServerMessage::Event { event }) => {
                        // No subscriber is not an error.
                        let _ = events.send(event);
                    }
                    Result::Ok(ServerMessage::Welcome { .. }) => {}
                    // A newer daemon of the same IPC version may send variants this client
                    // does not know (`#[non_exhaustive]`): ignore, do not fail.
                    Err(_) => tracing::debug!("ignoring an unrecognised daemon message"),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;
    use crate::{DaemonEvent, Status};

    fn status() -> Ok {
        Ok::Status(Status {
            device_id: "ab".repeat(32),
            short_id: "K3QD-7XPA".into(),
            addr: "127.0.0.1:42187".parse().expect("addr"),
            daemon_version: "test".into(),
            ipc_version: IPC_VERSION,
            settings: crate::Settings {
                receive_dir: "/tmp/in".into(),
                device_name: "alpha".into(),
                clipboard_sync: false,
                clipboard_images: true,
                clipboard_watch_supported: true,
                autostart: false,
                notifications: true,
            },
            add_device_window_open: false,
            locked: false,
        })
    }

    /// A handler that answers `Status` only after `release` fires, so a test can force an
    /// event to arrive while the request is in flight.
    fn gated_handler(release: Arc<tokio::sync::Notify>) -> Arc<dyn RequestHandler> {
        Arc::new(move |request: Request| {
            let release = Arc::clone(&release);
            async move {
                match request {
                    Request::Status => {
                        release.notified().await;
                        Response::Ok(status())
                    }
                    Request::Peers => Response::Ok(Ok::Peers(Vec::new())),
                    _ => Response::err(ErrorCode::NotFound, "unhandled"),
                }
            }
        })
    }

    #[tokio::test]
    async fn hello_request_and_interleaved_event() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (events, _) = broadcast::channel(16);
        let release = Arc::new(tokio::sync::Notify::new());
        let server = tokio::spawn(serve_connection(
            server_io,
            gated_handler(Arc::clone(&release)),
            events.subscribe(),
            "9.9.9",
        ));

        let client = PipeClient::handshake(client_io).await.expect("handshake");
        assert_eq!(client.daemon_version(), "9.9.9");
        let mut stream = client.events();

        let client = Arc::new(client);
        let slow = tokio::spawn({
            let client = Arc::clone(&client);
            async move { client.request(&Request::Status).await }
        });
        // A second request answers first although it was sent later: ids, not order, match.
        let peers = client.request(&Request::Peers).await.expect("peers");
        assert_eq!(peers, Ok::Peers(Vec::new()));

        // An event while `Status` is still pending.
        events
            .send(Event::Daemon(DaemonEvent::HostRestarted))
            .expect("send event");
        let got = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("event in time")
            .expect("stream open")
            .expect("not lagged");
        assert_eq!(got, Event::Daemon(DaemonEvent::HostRestarted));
        assert!(!slow.is_finished());

        release.notify_one();
        let answer = slow.await.expect("join").expect("status");
        assert_eq!(answer, status());

        let unknown = client.request(&Request::Shutdown).await;
        assert_eq!(unknown.expect_err("error").code, ErrorCode::NotFound);

        drop(stream);
        drop(client);
        let ended = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("server ends")
            .expect("join");
        assert!(ended.is_ok(), "clean close: {ended:?}");
    }

    #[tokio::test]
    async fn version_mismatch_is_refused() {
        let (client_io, server_io) = tokio::io::duplex(4096);
        let (events, _) = broadcast::channel(4);
        let release = Arc::new(tokio::sync::Notify::new());
        let server = tokio::spawn(serve_connection(
            server_io,
            gated_handler(release),
            events.subscribe(),
            "1.0.0",
        ));
        let (read, mut write) = tokio::io::split(client_io);
        write
            .write_all(b"{\"type\":\"hello\",\"ipc_version\":999}\n")
            .await
            .expect("write");
        let mut lines = BufReader::new(read).lines();
        let welcome = lines.next_line().await.expect("read").expect("welcome");
        assert!(welcome.contains("\"welcome\""));
        let refusal = lines.next_line().await.expect("read").expect("refusal");
        let parsed: ServerMessage = serde_json::from_str(&refusal).expect("parse");
        let ServerMessage::Response { id, response } = parsed else {
            panic!("expected a response");
        };
        assert_eq!(id, CONNECTION_ID);
        assert_eq!(response.error_code(), Some(ErrorCode::BadRequest));
        let result = server.await.expect("join");
        assert!(matches!(
            result,
            Err(PipeError::Version { client: 999, .. })
        ));
    }

    #[tokio::test]
    async fn oversized_line_drops_the_connection() {
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let (events, _) = broadcast::channel(4);
        let release = Arc::new(tokio::sync::Notify::new());
        let server = tokio::spawn(serve_connection(
            server_io,
            gated_handler(release),
            events.subscribe(),
            "1.0.0",
        ));
        let (read, mut write) = tokio::io::split(client_io);
        let writer = tokio::spawn(async move {
            write
                .write_all(b"{\"type\":\"hello\",\"ipc_version\":1}\n")
                .await?;
            let big = vec![b'x'; MAX_MESSAGE_BYTES + 16];
            write.write_all(&big).await?;
            write.write_all(b"\n").await
        });
        let mut lines = BufReader::new(read).lines();
        let welcome = lines.next_line().await.expect("read").expect("welcome");
        assert!(welcome.contains("\"welcome\""));
        let result = tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("server ends")
            .expect("join");
        assert!(matches!(result, Err(PipeError::TooLong)), "{result:?}");
        // The writer may see a broken pipe once the server drops its end; either is fine.
        let _ = writer.await;
    }

    #[tokio::test]
    async fn unknown_op_answers_bad_request_with_its_id() {
        let (client_io, server_io) = tokio::io::duplex(4096);
        let (events, _) = broadcast::channel(4);
        let release = Arc::new(tokio::sync::Notify::new());
        let _server = tokio::spawn(serve_connection(
            server_io,
            gated_handler(release),
            events.subscribe(),
            "1.0.0",
        ));
        let (read, mut write) = tokio::io::split(client_io);
        write
            .write_all(
                b"{\"type\":\"hello\",\"ipc_version\":1}\n{\"type\":\"request\",\"id\":7,\"request\":{\"op\":\"teleport\"}}\n",
            )
            .await
            .expect("write");
        let mut lines = BufReader::new(read).lines();
        let _welcome = lines.next_line().await.expect("read").expect("welcome");
        let answer = lines.next_line().await.expect("read").expect("answer");
        let parsed: ServerMessage = serde_json::from_str(&answer).expect("parse");
        let ServerMessage::Response { id, response } = parsed else {
            panic!("expected a response");
        };
        assert_eq!(id, 7);
        assert_eq!(response.error_code(), Some(ErrorCode::BadRequest));
    }

    #[tokio::test]
    async fn client_sees_daemon_going_away() {
        let (client_io, server_io) = tokio::io::duplex(4096);
        let (events, _) = broadcast::channel(4);
        let release = Arc::new(tokio::sync::Notify::new());
        let server = tokio::spawn(serve_connection(
            server_io,
            gated_handler(release),
            events.subscribe(),
            "1.0.0",
        ));
        let client = PipeClient::handshake(client_io).await.expect("handshake");
        let mut stream = client.events();
        // Closing the daemon's event channel ends the connection from the server side.
        drop(events);
        let _ = server.await;
        let end = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("stream ends in time");
        assert!(end.is_none());
        let err = client.request(&Request::Peers).await.expect_err("gone");
        assert_eq!(err.code, ErrorCode::Internal);
    }
}
