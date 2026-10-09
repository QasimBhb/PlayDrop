//! Named pipe client (`docs/ipc.md` section 3, SR-27). Untested: written on Linux, where
//! Windows code does not compile.

use std::path::PathBuf;
use std::time::Duration;

use tokio::net::windows::named_pipe::ClientOptions as PipeOptions;
use tokio_stream::wrappers::BroadcastStream;

use super::{is_daemon_image, ClientOptions, ConnectError};
use crate::pipe::{PipeClient, PipeError};
use crate::win::{
    current_user_sid, pipe_server_pid, process_image_path, process_user_sid, ERROR_FILE_NOT_FOUND,
    ERROR_PIPE_BUSY,
};
use crate::{Event, FilesOp, IpcError, Ok, Request, PIPE_PREFIX};

/// How often and how long to retry while every pipe instance is busy.
const BUSY_RETRIES: u32 = 20;
const BUSY_PAUSE: Duration = Duration::from_millis(50);

/// A verified pipe connection to the daemon.
pub(super) struct Client {
    pipe: PipeClient,
}

impl Client {
    pub(super) async fn connect(options: ClientOptions) -> Result<Self, ConnectError> {
        let own_sid = current_user_sid().map_err(|e| ConnectError::Transport(e.to_string()))?;
        let name = format!("{PIPE_PREFIX}{own_sid}");
        let mut tries = 0;
        let pipe = loop {
            match PipeOptions::new().open(&name) {
                Result::Ok(pipe) => break pipe,
                Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                    return Err(ConnectError::NotRunning)
                }
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < BUSY_RETRIES => {
                    tries += 1;
                    tokio::time::sleep(BUSY_PAUSE).await;
                }
                Err(e) => return Err(ConnectError::Transport(e.to_string())),
            }
        };

        // SR-27: the server must run as our user and be `playdropd.exe`.
        let unverified = |e: std::io::Error| ConnectError::Unverified(e.to_string());
        let pid = pipe_server_pid(&pipe).map_err(unverified)?;
        let sid = process_user_sid(pid).map_err(unverified)?;
        if sid != own_sid {
            tracing::warn!("ipc daemon refused: another user owns the pipe");
            return Err(ConnectError::Unverified(
                "the daemon pipe is owned by another user".to_owned(),
            ));
        }
        if options.verify_exe {
            let image = process_image_path(pid).map_err(unverified)?;
            if !is_daemon_image(&image) {
                tracing::warn!("ipc daemon refused: pipe server is not playdropd.exe");
                return Err(ConnectError::Unverified(format!(
                    "the daemon pipe is served by {image}, not playdropd.exe"
                )));
            }
        }

        let pipe = PipeClient::handshake(pipe).await.map_err(|e| match e {
            PipeError::Version { daemon, client } => ConnectError::Version { daemon, client },
            other => ConnectError::Transport(other.to_string()),
        })?;
        Result::Ok(Self { pipe })
    }

    pub(super) fn daemon_version(&self) -> &str {
        self.pipe.daemon_version()
    }

    pub(super) async fn request(&self, request: &Request) -> Result<Ok, IpcError> {
        self.pipe.request(request).await
    }

    /// Paths travel as is on Windows; the pipe's DACL and SID checks are the boundary.
    pub(super) async fn files(
        &self,
        op: FilesOp,
        peer: &str,
        paths: &[PathBuf],
    ) -> Result<Ok, IpcError> {
        self.pipe
            .request(&op.request(peer.to_owned(), paths.to_vec()))
            .await
    }

    pub(super) fn events(&self) -> BroadcastStream<Event> {
        self.pipe.events()
    }
}
