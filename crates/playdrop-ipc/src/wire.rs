//! Message framing shared by both transports.
//!
//! D-Bus carries one JSON document per method argument, so framing there is the bus's own.
//! The Windows named pipe is a byte stream and uses newline-delimited JSON: one
//! [`ClientMessage`] or [`ServerMessage`] per line, UTF-8, no embedded newlines (`serde_json`
//! never emits them), each line at most [`crate::MAX_MESSAGE_BYTES`].
//!
//! Requests carry a client-chosen `id` so responses can be matched while events interleave.

use serde::{Deserialize, Serialize};

use crate::{Event, Request, Response};

/// A line from a client to the daemon (pipe transport).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First line on a connection: the IPC version the client speaks.
    Hello {
        /// [`crate::IPC_VERSION`] of the client.
        ipc_version: u32,
    },
    /// A request.
    Request {
        /// Client-chosen correlation ID.
        id: u64,
        /// The request.
        request: Request,
    },
}

/// A line from the daemon to a client (pipe transport).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Answer to the client's hello.
    Welcome {
        /// [`crate::IPC_VERSION`] of the daemon.
        ipc_version: u32,
        /// Daemon package version.
        daemon_version: String,
    },
    /// Answer to a request.
    Response {
        /// The request's correlation ID.
        id: u64,
        /// The answer.
        response: Response,
    },
    /// An unprompted event.
    Event {
        /// The event.
        event: Event,
    },
}
