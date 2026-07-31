use tokio::sync::mpsc;
use super::*;

/// In-memory transport for tests and in-process component wiring.
///
/// Created via [`MemoryTransport::channel()`] which returns a
/// `(MemoryTransport, MemoryHandle)` pair connected by mpsc channels.
///
/// - `MemoryTransport` implements [`PacketTransport`] and can be wrapped in a session (see `network`).
/// - [`MemoryHandle`] stays with the caller for direct packet send/recv.
///
/// ```text
/// handle.send() ──tx──► transport.recv()   (external code → session)
/// transport.send() ──tx──► handle.recv()   (session → external code)
/// ```
#[derive(Debug)]
pub struct MemoryTransport {
    rx: mpsc::Receiver<TransportEvent>,
    tx: mpsc::Sender<TransportEvent>,
}

/// External handle for interacting with a session through memory channels.
///
/// Obtained from [`MemoryTransport::channel()`]. Allows sending events
/// into the session and receiving events that the session sends out.
#[derive(Debug)]
pub struct MemoryHandle {
    tx: mpsc::Sender<TransportEvent>,
    rx: mpsc::Receiver<TransportEvent>,
}

impl MemoryTransport {
    /// Create a `(MemoryTransport, MemoryHandle)` pair connected by channels.
    ///
    /// `buffer` is the capacity of each mpsc channel.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use protocol::transport::memory::MemoryTransport;
    ///
    /// let (transport, mut handle) = MemoryTransport::channel(32);
    ///
    /// // transport implements PacketTransport — wrap it in a Session (network)
    /// // or use directly via recv()/send()
    ///
    /// // handle stays with external code
    /// // handle.send(...) feeds events into the transport
    /// // handle.recv() reads events the transport sends out
    /// ```
    pub fn channel(buffer: usize) -> (Self, MemoryHandle) {
        // external → transport (handle.send → transport.recv)
        let (handle_tx, transport_rx) = mpsc::channel(buffer);
        // transport → external (transport.send → handle.recv)
        let (transport_tx, handle_rx) = mpsc::channel(buffer);

        let transport = Self {
            rx: transport_rx,
            tx: transport_tx,
        };

        let handle = MemoryHandle {
            tx: handle_tx,
            rx: handle_rx,
        };

        (transport, handle)
    }
}

#[async_trait]
impl PacketTransport for MemoryTransport {
    async fn recv(&mut self) -> Result<TransportEvent, TransportError> {
        self.rx.recv().await.ok_or(TransportError::Closed)
    }

    async fn send(&mut self, event: TransportEvent) -> Result<(), TransportError> {
        self.tx.send(event).await.map_err(|_| TransportError::Closed)
    }

    async fn close(&mut self) {
        self.rx.close();
    }
}

impl MemoryHandle {
    /// Send an event into the session (the session will receive it via `recv()`).
    pub async fn send(&self, event: TransportEvent) -> Result<(), TransportError> {
        self.tx.send(event).await.map_err(|_| TransportError::Closed)
    }

    /// Receive an event from the session (sent by the session via `send()`).
    pub async fn recv(&mut self) -> Result<TransportEvent, TransportError> {
        self.rx.recv().await.ok_or(TransportError::Closed)
    }

    /// Close the handle, signaling the transport that no more events will be sent.
    pub fn close(&mut self) {
        self.rx.close();
    }
}
