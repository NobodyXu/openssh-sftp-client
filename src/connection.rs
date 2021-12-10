use super::awaitable_responses::AwaitableResponses;
use super::*;

use core::fmt::Debug;
use core::marker::Unpin;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::sync::Notify;

use tokio::io::{AsyncRead, AsyncWrite};

use openssh_sftp_protocol::constants::SSH2_FILEXFER_VERSION;

/// TODO:
///  - Support for zero copy API

/// SharedData contains both the writer and the responses because:
///  - The overhead of `Arc` and a separate allocation;
///  - If the write end of a connection is closed, then openssh implementation
///    of sftp-server would close the read end right away, discarding
///    any unsent but processed or unprocessed responses.
#[derive(Debug)]
pub(crate) struct SharedData<Writer: AsyncWrite + Unpin, Buffer: ToBuffer + 'static> {
    pub(crate) writer: Mutex<Writer>,
    pub(crate) responses: AwaitableResponses<Buffer>,

    notify: Notify,
    requests_sent: AtomicUsize,
}

impl<Writer: AsyncWrite + Unpin, Buffer: ToBuffer + 'static> SharedData<Writer, Buffer> {
    pub(crate) fn notify_new_packet_event(&self) {
        self.requests_sent.fetch_add(1, Ordering::Relaxed);

        // We only have one waiting task, that is `ReadEnd`.
        // Notify the `ReadEnd` after the requests_sent is incremented.
        self.notify.notify_one();
    }

    /// Return number of requests and clear requests_sent
    pub(crate) async fn wait_for_new_request(&self) -> usize {
        loop {
            let cnt = self.requests_sent.swap(0, Ordering::Relaxed);
            if cnt > 0 {
                break cnt;
            }
            self.notify.notified().await;
        }
    }
}

pub async fn connect<
    Buffer: ToBuffer + Debug + Send + Sync + 'static,
    Writer: AsyncWrite + Unpin,
    Reader: AsyncRead + Unpin,
>(
    reader: Reader,
    writer: Writer,
) -> Result<(WriteEnd<Writer, Buffer>, ReadEnd<Writer, Reader, Buffer>), Error> {
    let shared_data = Arc::new(SharedData {
        writer: Mutex::new(writer),
        responses: AwaitableResponses::new(),
        notify: Notify::new(),
        requests_sent: AtomicUsize::new(0),
    });

    let mut read_end = ReadEnd::new(reader, shared_data.clone());
    let mut write_end = WriteEnd::new(shared_data);

    // negotiate
    let version = SSH2_FILEXFER_VERSION;

    write_end.send_hello(version, Default::default()).await?;
    read_end.receive_server_version(version).await?;

    Ok((write_end, read_end))
}

#[cfg(test)]
mod tests {
    use crate::*;

    use std::path;
    use std::process::Stdio;

    use once_cell::sync::OnceCell;

    use tokio::process;

    fn get_sftp_path() -> &'static path::Path {
        static SFTP_PATH: OnceCell<path::PathBuf> = OnceCell::new();

        SFTP_PATH.get_or_init(|| {
            let mut sftp_path = path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            sftp_path.push("static-openssh-sftp-server");
            sftp_path.push("sftp-server");

            sftp_path
        })
    }

    async fn launch_sftp() -> (process::Child, process::ChildStdin, process::ChildStdout) {
        let mut child = process::Command::new(get_sftp_path())
            .args(&["-e", "-l", "DEBUG"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        (child, stdin, stdout)
    }

    async fn connect() -> (
        WriteEnd<process::ChildStdin, Vec<u8>>,
        ReadEnd<process::ChildStdin, process::ChildStdout, Vec<u8>>,
        process::Child,
    ) {
        let (child, stdin, stdout) = launch_sftp().await;
        let (write_end, read_end) = crate::connect(stdout, stdin).await.unwrap();
        (write_end, read_end, child)
    }

    #[tokio::test]
    async fn test_connect() {
        let mut child = connect().await.2;
        assert!(child.wait().await.unwrap().success());
    }
}
