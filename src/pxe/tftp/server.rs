//! A read-only TFTP server, which is all a network boot server needs.
//!
//! There is no write support and no way to add any. TFTP has no
//! authentication of any kind, so a writable TFTP server on a boot network is
//! a way for anything on that network to replace the boot loader every machine
//! is about to execute. Read requests are served; write requests get an error
//! naming the file, which is the useful half of the answer.
//!
//! Every transfer gets its own socket, as RFC 1350 requires: the server's
//! well-known port only ever carries the first packet of a conversation, and
//! the rest happens between two ephemeral ports.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::UdpSocket;

use super::packet::{self, ErrorCode, Packet, ReadRequest, DEFAULT_BLOCK_SIZE};

/// How a transfer ended, for the boot log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    Served { bytes: u64 },
    NotFound,
    /// The path pointed outside the root, or the mode was not `octet`.
    Denied(&'static str),
    Failed(String),
}

/// What the application wants to know about.
#[async_trait::async_trait]
pub trait TftpEvents: Send + Sync + 'static {
    async fn read(&self, filename: String, peer: SocketAddr, outcome: ReadOutcome);
}

/// An observer that does nothing, for tests and for a server with no inventory.
pub struct NoEvents;

#[async_trait::async_trait]
impl TftpEvents for NoEvents {
    async fn read(&self, _filename: String, _peer: SocketAddr, _outcome: ReadOutcome) {}
}

/// The bounds a client may negotiate within.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The largest block a client may ask for.
    ///
    /// 1468 keeps a 1500-byte Ethernet frame intact: 1500 less 20 bytes of IP,
    /// 8 of UDP and 4 of TFTP header. Going above it fragments every packet,
    /// which on the kind of switch a boot network runs on is slower than the
    /// smaller block would have been.
    pub max_block_size: u16,
    /// The most blocks that may be in flight before an acknowledgement
    /// (RFC 7440). This is the difference between a 300MB image taking two
    /// minutes and taking twenty.
    pub max_window_size: u16,
    pub timeout: Duration,
    pub retries: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_block_size: 1468,
            max_window_size: 16,
            timeout: Duration::from_secs(3),
            retries: 5,
        }
    }
}

pub struct TftpServer {
    root: PathBuf,
    events: Arc<dyn TftpEvents>,
    limits: Limits,
}

impl TftpServer {
    pub fn new(root: impl Into<PathBuf>, events: Arc<dyn TftpEvents>) -> Self {
        Self { root: root.into(), events, limits: Limits::default() }
    }

    #[must_use = "this returns a configured server rather than configuring in place"]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Take the port, without serving on it yet.
    ///
    /// Separate from [`Bound::serve`] so a server with several listeners can
    /// discover that one of them cannot bind *before* any of them start
    /// answering. Half a boot server is worse than none: a machine offered a
    /// file by a listener that came up, which then cannot fetch it from one
    /// that did not, hangs with no message.
    pub async fn bind(self, address: SocketAddr) -> std::io::Result<Bound> {
        let socket = UdpSocket::bind(address).await.map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "could not bind {address} for TFTP: {e}. Port 69 needs root (or \
                     CAP_NET_BIND_SERVICE); on Windows, an elevated prompt."
                ),
            )
        })?;

        Ok(Bound { server: Arc::new(self), socket })
    }

    /// Bind and serve until the process stops.
    pub async fn run(self, bind: SocketAddr) -> std::io::Result<()> {
        self.bind(bind).await?.serve().await
    }

    async fn dispatch(self: Arc<Self>, datagram: &[u8], peer: SocketAddr) {
        match packet::decode(datagram) {
            Ok(Packet::Read(request)) => self.serve(request, peer).await,
            Ok(Packet::Write { filename }) => {
                tracing::warn!(%peer, filename, "refused a TFTP write: this server is read-only");
                self.events
                    .read(filename, peer, ReadOutcome::Denied("this TFTP server is read-only"))
                    .await;
                let _ = reply_error(
                    peer,
                    ErrorCode::AccessViolation,
                    "this TFTP server is read-only",
                )
                .await;
            }
            Ok(_) => {
                // A DATA or ACK arriving at the well-known port belongs to a
                // transfer that has already moved to its own socket, or to no
                // transfer at all.
                tracing::trace!(%peer, "ignoring a stray TFTP packet on the listening port");
            }
            Err(e) => tracing::debug!(%peer, error = %e, "ignoring a datagram that is not TFTP"),
        }
    }

    async fn serve(&self, request: ReadRequest, peer: SocketAddr) {
        let filename = packet::normalise_filename(&request.filename);

        if !request.is_octet() {
            // `netascii` would line-ending-convert a kernel image into
            // something that does not boot, silently. Refusing is kinder.
            self.events
                .read(filename, peer, ReadOutcome::Denied("only `octet` mode is served"))
                .await;
            let _ = reply_error(peer, ErrorCode::IllegalOperation, "only `octet` mode is served")
                .await;
            return;
        }

        let Some(path) = resolve(&self.root, &filename) else {
            tracing::warn!(%peer, filename, "refused a TFTP path that left the root");
            self.events
                .read(filename, peer, ReadOutcome::Denied("that path is outside the TFTP root"))
                .await;
            let _ =
                reply_error(peer, ErrorCode::AccessViolation, "that path is outside the root").await;
            return;
        };

        match self.transfer(&path, &request, peer).await {
            Ok(bytes) => {
                tracing::info!(%peer, filename, bytes, "served over TFTP");
                self.events.read(filename, peer, ReadOutcome::Served { bytes }).await;
            }
            Err(TransferError::NotFound) => {
                tracing::info!(%peer, filename, path = %path.display(), "TFTP file not found");
                self.events.read(filename.clone(), peer, ReadOutcome::NotFound).await;
                let _ = reply_error(
                    peer,
                    ErrorCode::FileNotFound,
                    &format!("no such file: {filename}"),
                )
                .await;
            }
            Err(TransferError::Io(e)) => {
                tracing::warn!(%peer, filename, error = %e, "TFTP transfer failed");
                self.events.read(filename, peer, ReadOutcome::Failed(e.to_string())).await;
            }
            Err(TransferError::Timeout) => {
                tracing::info!(%peer, filename, "TFTP transfer timed out");
                self.events
                    .read(filename, peer, ReadOutcome::Failed("timed out".to_string()))
                    .await;
            }
            Err(TransferError::Aborted(why)) => {
                tracing::info!(%peer, filename, why, "TFTP transfer abandoned");
                self.events.read(filename, peer, ReadOutcome::Failed(why)).await;
            }
        }
    }

    /// One file, on its own socket.
    async fn transfer(
        &self,
        path: &Path,
        request: &ReadRequest,
        peer: SocketAddr,
    ) -> Result<u64, TransferError> {
        let mut file = match tokio::fs::File::open(path).await {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(TransferError::NotFound)
            }
            Err(e) => return Err(TransferError::Io(e)),
        };

        let size = file.metadata().await.map_err(TransferError::Io)?.len();

        // RFC 1350: the transfer moves to a fresh port immediately. Connecting
        // it means a packet from anywhere else is dropped by the kernel rather
        // than mistaken for the client's.
        let socket = UdpSocket::bind(bind_any(peer)).await.map_err(TransferError::Io)?;
        socket.connect(peer).await.map_err(TransferError::Io)?;

        let (granted, block_size, window_size, timeout) = self.negotiate(request, size);

        if !granted.is_empty() {
            socket.send(&packet::oack(&granted)).await.map_err(TransferError::Io)?;
            // The client acknowledges an OACK with block 0, and only then does
            // the data start. Sending block 1 first would be a server that
            // negotiated and then ignored the answer.
            wait_for_ack(&socket, 0, timeout, self.retries()).await?;
        }

        // A file whose length is an exact multiple of the block size needs a
        // final *empty* block, because "short block" is how TFTP says "that
        // was the last one". This is the integer division that gets that
        // right for every length including zero.
        let total_blocks = size / block_size as u64 + 1;

        let mut base: u64 = 0; // the first block not yet acknowledged
        let mut sent: u64 = 0; // the first block not yet sent
        let mut retries = 0u32;
        let mut buffer = vec![0u8; block_size as usize];

        while base < total_blocks {
            while sent < total_blocks && sent - base < window_size as u64 {
                let payload =
                    read_block(&mut file, sent, block_size, &mut buffer).await.map_err(TransferError::Io)?;
                socket
                    .send(&packet::data(block_number(sent), payload))
                    .await
                    .map_err(TransferError::Io)?;
                sent += 1;
            }

            match recv_packet(&socket, timeout).await {
                Ok(Some(Packet::Ack { block })) => {
                    match acknowledged_index(block, base, sent) {
                        Some(index) => {
                            base = index + 1;
                            retries = 0;
                        }
                        // An acknowledgement for something outside the window
                        // is a duplicate of one already handled. Ignoring it
                        // is what stops the "Sorcerer's Apprentice" bug, where
                        // each side answers the other's retransmission
                        // forever.
                        None => continue,
                    }
                }
                Ok(Some(Packet::Error { code, message })) => {
                    return Err(TransferError::Aborted(format!(
                        "the client sent error {code}: {message}"
                    )))
                }
                Ok(Some(_)) | Ok(None) => continue,
                Err(TransferError::Timeout) => {
                    retries += 1;
                    if retries > self.limits.retries {
                        return Err(TransferError::Aborted(
                            "the client stopped acknowledging".to_string(),
                        ));
                    }
                    // Rewind to the first unacknowledged block. Everything
                    // after it is resent, because the window's contents are
                    // exactly what may have been lost.
                    sent = base;
                }
                Err(e) => return Err(e),
            }
        }

        Ok(size)
    }

    fn retries(&self) -> u32 {
        self.limits.retries
    }

    /// Answer the options this server is willing to honour, and only those.
    ///
    /// An option acknowledged is an option that must then be obeyed exactly,
    /// so anything out of range is granted at the clamped value rather than
    /// echoed back — a client asking for a 64KB block and being told "yes"
    /// would wait for packets that never fit on the wire.
    fn negotiate(
        &self,
        request: &ReadRequest,
        size: u64,
    ) -> (BTreeMap<String, String>, u16, u16, Duration) {
        let mut granted = BTreeMap::new();

        let block_size = match request.option_u64("blksize") {
            Some(asked) => {
                let granted_size =
                    (asked.clamp(8, self.limits.max_block_size as u64)) as u16;
                granted.insert("blksize".to_string(), granted_size.to_string());
                granted_size
            }
            None => DEFAULT_BLOCK_SIZE,
        };

        // `tsize` with a zero value means "tell me how big it is", which is
        // how iPXE draws a progress bar.
        if request.options.contains_key("tsize") {
            granted.insert("tsize".to_string(), size.to_string());
        }

        let timeout = match request.option_u64("timeout") {
            Some(seconds) => {
                let seconds = seconds.clamp(1, 255);
                granted.insert("timeout".to_string(), seconds.to_string());
                Duration::from_secs(seconds)
            }
            None => self.limits.timeout,
        };

        let window_size = match request.option_u64("windowsize") {
            Some(asked) => {
                let granted_window = (asked.clamp(1, self.limits.max_window_size as u64)) as u16;
                granted.insert("windowsize".to_string(), granted_window.to_string());
                granted_window
            }
            None => 1,
        };

        (granted, block_size, window_size, timeout)
    }
}

/// A TFTP server holding its port.
pub struct Bound {
    server: Arc<TftpServer>,
    socket: UdpSocket,
}

impl Bound {
    /// The address actually bound — how a test finds an ephemeral port, and
    /// how a log line reports what port 0 became.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn serve(self) -> std::io::Result<()> {
        tracing::info!(
            address = %self.socket.local_addr().map(|a| a.to_string()).unwrap_or_default(),
            root = %self.server.root.display(),
            "TFTP listening (read-only)"
        );

        let mut buffer = vec![0u8; 2048];

        loop {
            let (length, peer) = match self.socket.recv_from(&mut buffer).await {
                Ok(received) => received,
                Err(e) => {
                    tracing::warn!(error = %e, "TFTP recv failed; continuing");
                    continue;
                }
            };

            let datagram = buffer[..length].to_vec();
            let server = Arc::clone(&self.server);
            tokio::spawn(async move { server.dispatch(&datagram, peer).await });
        }
    }
}

#[derive(Debug)]
enum TransferError {
    NotFound,
    Io(std::io::Error),
    /// The client stopped answering. Its own variant rather than an `Aborted`
    /// carrying the word, because the retransmit loop branches on it.
    Timeout,
    Aborted(String),
}

/// Block numbers start at 1 and wrap through zero, which is what makes a file
/// larger than 32MB transferable at the default block size.
fn block_number(index: u64) -> u16 {
    ((index + 1) % 65536) as u16
}

/// Which in-flight block an acknowledgement refers to.
///
/// Block numbers wrap, so the number alone is ambiguous; the window is what
/// disambiguates it. Anything outside the window is a duplicate.
fn acknowledged_index(block: u16, base: u64, sent: u64) -> Option<u64> {
    (base..sent).find(|index| block_number(*index) == block)
}

async fn read_block<'a>(
    file: &mut tokio::fs::File,
    index: u64,
    block_size: u16,
    buffer: &'a mut [u8],
) -> std::io::Result<&'a [u8]> {
    let offset = index * block_size as u64;
    file.seek(std::io::SeekFrom::Start(offset)).await?;

    let mut filled = 0usize;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]).await? {
            0 => break,
            read => filled += read,
        }
    }
    Ok(&buffer[..filled])
}

async fn recv_packet(
    socket: &UdpSocket,
    timeout: Duration,
) -> Result<Option<Packet>, TransferError> {
    let mut buffer = [0u8; 1024];
    match tokio::time::timeout(timeout, socket.recv(&mut buffer)).await {
        Err(_) => Err(TransferError::Timeout),
        Ok(Err(e)) => Err(TransferError::Io(e)),
        Ok(Ok(length)) => Ok(packet::decode(&buffer[..length]).ok()),
    }
}

async fn wait_for_ack(
    socket: &UdpSocket,
    block: u16,
    timeout: Duration,
    retries: u32,
) -> Result<(), TransferError> {
    for _ in 0..=retries {
        match recv_packet(socket, timeout).await {
            Ok(Some(Packet::Ack { block: acked })) if acked == block => return Ok(()),
            Ok(_) => continue,
            Err(TransferError::Timeout) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(TransferError::Aborted(
        "the client never acknowledged the negotiated options".to_string(),
    ))
}

/// Errors are sent from a throwaway socket, because the transfer's own socket
/// may not exist yet — the refusal happens before the file is even opened.
async fn reply_error(peer: SocketAddr, code: ErrorCode, message: &str) -> std::io::Result<()> {
    let socket = UdpSocket::bind(bind_any(peer)).await?;
    socket.send_to(&packet::error(code, message), peer).await?;
    Ok(())
}

fn bind_any(peer: SocketAddr) -> SocketAddr {
    match peer {
        SocketAddr::V4(_) => "0.0.0.0:0".parse().expect("a literal address"),
        SocketAddr::V6(_) => "[::]:0".parse().expect("a literal address"),
    }
}

/// Turn a requested name into a path inside the root, or nothing.
///
/// Traversal is refused by construction: only ordinary path components are
/// kept, and a component beginning with `.` — which covers `..`, `.git` and
/// every dotfile — ends the walk. The caller still gets a path that may not
/// exist; that is the next check, and it is a different answer.
pub fn resolve(root: &Path, filename: &str) -> Option<PathBuf> {
    let filename = packet::normalise_filename(filename);
    if filename.is_empty() {
        return None;
    }

    let mut safe = PathBuf::new();
    for component in Path::new(&filename).components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str()?;
                if part.starts_with('.') {
                    return None;
                }
                safe.push(part);
            }
            // An absolute path, a drive letter, a `..` or a `.` — none of them
            // can appear in a name that means a file under the root.
            Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_)
            | Component::CurDir => return None,
        }
    }

    if safe.as_os_str().is_empty() {
        return None;
    }
    Some(root.join(safe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_inside_the_root_resolves() {
        let root = Path::new("/srv/tftp");
        assert_eq!(
            resolve(root, "ipxe/undionly.kpxe"),
            Some(PathBuf::from("/srv/tftp").join("ipxe").join("undionly.kpxe"))
        );
    }

    #[test]
    fn traversal_is_refused_however_it_is_spelled() {
        // The one that matters. A TFTP server on a boot network is reachable
        // by anything that can send a UDP packet.
        let root = Path::new("/srv/tftp");
        for attempt in [
            "../etc/shadow",
            "ipxe/../../etc/shadow",
            "/../etc/shadow",
            "\\..\\..\\windows\\system32\\config\\sam",
            "./secret",
            ".env",
            "ipxe/.ssh/id_rsa",
            "",
        ] {
            assert_eq!(resolve(root, attempt), None, "`{attempt}` should be refused");
        }
    }

    #[test]
    fn a_leading_slash_means_the_root_rather_than_the_filesystem() {
        // Firmware writes `/ipxe.efi` and means "the file this server serves",
        // not `/ipxe.efi` on the server's disk. Stripping the slash is what
        // every TFTP server does; what would be wrong is letting the name
        // escape afterwards, which the traversal test above covers.
        let root = Path::new("/srv/tftp");
        assert_eq!(resolve(root, "/ipxe.efi"), Some(root.join("ipxe.efi")));
        assert_eq!(resolve(root, "/etc/shadow"), Some(root.join("etc").join("shadow")));
    }

    #[test]
    fn a_windows_drive_letter_is_not_a_relative_path() {
        // On Windows, `C:file` is a path relative to the current directory of
        // drive C — which is not under the root, and `join` would not make it
        // so.
        assert_eq!(resolve(Path::new("/srv/tftp"), "C:/windows/system32/config/sam"), None);
    }

    #[test]
    fn block_numbers_start_at_one_and_wrap_through_zero() {
        // Without the wrap, no file over 32MB transfers at the default block
        // size.
        assert_eq!(block_number(0), 1);
        assert_eq!(block_number(65534), 65535);
        assert_eq!(block_number(65535), 0);
        assert_eq!(block_number(65536), 1);
    }

    #[test]
    fn an_acknowledgement_is_matched_to_the_window_it_belongs_to() {
        // The number alone is ambiguous once it has wrapped; the window is
        // what resolves it.
        assert_eq!(acknowledged_index(1, 0, 4), Some(0));
        assert_eq!(acknowledged_index(4, 0, 4), Some(3));
        assert_eq!(acknowledged_index(9, 0, 4), None, "outside the window: a duplicate");

        // Across the wrap.
        assert_eq!(acknowledged_index(0, 65534, 65538), Some(65535));
        assert_eq!(acknowledged_index(1, 65534, 65538), Some(65536));
    }

    #[test]
    fn a_file_that_is_an_exact_multiple_of_the_block_size_gets_a_final_empty_block() {
        // The subtlest bug in TFTP: without the extra block the client waits
        // forever for a short one that never comes.
        let blocks = |size: u64, block: u64| size / block + 1;
        assert_eq!(blocks(0, 512), 1, "an empty file is one empty block");
        assert_eq!(blocks(100, 512), 1);
        assert_eq!(blocks(512, 512), 2, "the second one is empty and ends the transfer");
        assert_eq!(blocks(1024, 512), 3);
        assert_eq!(blocks(513, 512), 2);
    }

    fn request(options: &[(&str, &str)]) -> ReadRequest {
        ReadRequest {
            filename: "ipxe.efi".into(),
            mode: "octet".into(),
            options: options
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        }
    }

    fn server() -> TftpServer {
        TftpServer::new("/srv/tftp", Arc::new(NoEvents))
    }

    #[test]
    fn asking_for_nothing_negotiates_nothing() {
        let (granted, block, window, _) = server().negotiate(&request(&[]), 1000);
        assert!(granted.is_empty(), "no OACK is sent at all");
        assert_eq!(block, DEFAULT_BLOCK_SIZE);
        assert_eq!(window, 1);
    }

    #[test]
    fn a_block_size_beyond_the_frame_is_granted_at_the_value_that_fits() {
        // Echoing 65464 back would be a promise to send packets that fragment
        // on every switch between here and the client.
        let (granted, block, _, _) = server().negotiate(&request(&[("blksize", "65464")]), 1000);
        assert_eq!(granted.get("blksize").map(String::as_str), Some("1468"));
        assert_eq!(block, 1468);
    }

    #[test]
    fn a_tiny_block_size_is_floored_rather_than_accepted() {
        let (granted, block, _, _) = server().negotiate(&request(&[("blksize", "1")]), 1000);
        assert_eq!(granted.get("blksize").map(String::as_str), Some("8"));
        assert_eq!(block, 8);
    }

    #[test]
    fn tsize_is_answered_with_the_size_so_the_client_can_draw_a_progress_bar() {
        let (granted, _, _, _) = server().negotiate(&request(&[("tsize", "0")]), 1_048_576);
        assert_eq!(granted.get("tsize").map(String::as_str), Some("1048576"));
    }

    #[test]
    fn a_window_is_granted_within_the_limit() {
        let (granted, _, window, _) = server().negotiate(&request(&[("windowsize", "8")]), 1000);
        assert_eq!(window, 8);
        assert_eq!(granted.get("windowsize").map(String::as_str), Some("8"));

        let (granted, _, window, _) = server().negotiate(&request(&[("windowsize", "999")]), 1000);
        assert_eq!(window, 16, "clamped to what this server will do");
        assert_eq!(granted.get("windowsize").map(String::as_str), Some("16"));
    }

    #[test]
    fn a_timeout_is_clamped_into_the_range_the_option_allows() {
        let (granted, _, _, timeout) = server().negotiate(&request(&[("timeout", "0")]), 1000);
        assert_eq!(timeout, Duration::from_secs(1));
        assert_eq!(granted.get("timeout").map(String::as_str), Some("1"));

        let (_, _, _, timeout) = server().negotiate(&request(&[("timeout", "9000")]), 1000);
        assert_eq!(timeout, Duration::from_secs(255));
    }

    #[test]
    fn an_option_this_server_does_not_know_is_not_acknowledged() {
        // RFC 2347: options not in the OACK are simply not in effect. Echoing
        // one back would promise behaviour that does not exist.
        let (granted, _, _, _) = server().negotiate(&request(&[("rollover", "0")]), 1000);
        assert!(granted.is_empty());
    }
}
