use russh::ChannelId;
use russh::server::Handle;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

/// Adapts an SSH channel into a `std::io::Write` sink, so ratatui's
/// `CrosstermBackend` (which just wants somewhere to write ANSI bytes) can
/// render straight into the client's terminal over the SSH connection.
pub struct TerminalHandle {
    sender: UnboundedSender<Vec<u8>>,
    sink: Vec<u8>,
}

impl TerminalHandle {
    pub async fn start(handle: Handle, channel_id: ChannelId) -> Self {
        let (sender, mut receiver) = unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(data) = receiver.recv().await {
                // Fails once the client has gone away; nothing left to send to.
                if handle.data(channel_id, data).await.is_err() {
                    break;
                }
            }
        });
        Self {
            sender,
            sink: Vec::new(),
        }
    }
}

impl TerminalHandle {
    /// Another writer into the same SSH channel (with its own buffer).
    pub fn duplicate(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            sink: Vec::new(),
        }
    }
}

impl std::io::Write for TerminalHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.sink.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Err(err) = self.sender.send(std::mem::take(&mut self.sink)) {
            return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, err));
        }
        Ok(())
    }
}
