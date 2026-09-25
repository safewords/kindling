//! TFTP over a real socket.
//!
//! The packet encoding is unit-tested next to the code; what is tested here is
//! the *transfer loop*, because that is where TFTP's genuinely subtle
//! behaviour lives — the final short block, the extra empty one, the move to a
//! second port, the options that must be honoured exactly once acknowledged.
//! None of it can be exercised without a socket.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pxe::pxe::tftp::packet::{self, Packet};
use pxe::pxe::tftp::server::{NoEvents, TftpServer};
use tokio::net::UdpSocket;

/// A server on an ephemeral port, serving a directory this test owns.
struct Served {
    address: SocketAddr,
    root: PathBuf,
}

impl Served {
    async fn start(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("kindling-tftp-{}-{name}-{}", std::process::id(), nanos()));
        std::fs::create_dir_all(&root).expect("a directory to serve");

        let bound = TftpServer::new(&root, Arc::new(NoEvents))
            .bind("127.0.0.1:0".parse().unwrap())
            .await
            .expect("an ephemeral port");

        let address = bound.local_addr().expect("the port it took");
        tokio::spawn(async move { bound.serve().await });

        Self { address, root }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory");
        }
        std::fs::write(path, bytes).expect("a file to serve");
    }

    /// Fetch a file the way a client does, returning the bytes and whatever
    /// options the server agreed to.
    async fn fetch(
        &self,
        filename: &str,
        options: &[(&str, &str)],
    ) -> Result<(Vec<u8>, BTreeMap<String, String>), (u16, String)> {
        let client = UdpSocket::bind("127.0.0.1:0").await.expect("a client socket");
        client.send_to(&read_request(filename, options), self.address).await.expect("sent");

        let mut buffer = vec![0u8; 4096];
        let mut received = Vec::new();
        let mut agreed = BTreeMap::new();
        let mut block_size = 512usize;
        let mut window = 1u64;
        let mut in_window = 0u64;

        // The transfer moves to a port of the server's choosing with its first
        // reply; everything after that is between the two ephemeral ports.
        let mut transfer: Option<SocketAddr> = None;

        loop {
            let (length, peer) =
                tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buffer))
                    .await
                    .expect("the server answered in time")
                    .expect("a datagram");

            let from = *transfer.get_or_insert(peer);
            assert_eq!(peer, from, "a transfer stays on the port it started on");
            assert_ne!(peer, self.address, "and that port is not the listening one");

            match packet::decode(&buffer[..length]).expect("a TFTP packet") {
                Packet::Oack { options } => {
                    if let Some(size) = options.get("blksize") {
                        block_size = size.parse().expect("a number");
                    }
                    if let Some(size) = options.get("windowsize") {
                        window = size.parse().expect("a number");
                    }
                    agreed = options;
                    // An OACK is acknowledged with block zero, and only then
                    // does the data start.
                    client.send_to(&packet::ack(0), from).await.expect("sent");
                }
                Packet::Data { block, data } => {
                    let last = data.len() < block_size;
                    received.extend_from_slice(&data);
                    in_window += 1;

                    if last || in_window >= window {
                        client.send_to(&packet::ack(block), from).await.expect("sent");
                        in_window = 0;
                    }

                    if last {
                        return Ok((received, agreed));
                    }
                }
                Packet::Error { code, message } => return Err((code, message)),
                other => panic!("unexpected packet: {other:?}"),
            }
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

fn read_request(filename: &str, options: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(filename.as_bytes());
    out.push(0);
    out.extend_from_slice(b"octet");
    out.push(0);
    for (name, value) in options {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }
    out
}

/// Something that looks like a boot loader rather than a run of one byte, so a
/// corrupted transfer is visible as garbage rather than as a shorter run.
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[tokio::test]
async fn a_small_file_arrives_intact() {
    let served = Served::start("small").await;
    let bytes = payload(100);
    served.write("undionly.kpxe", &bytes);

    let (received, agreed) = served.fetch("undionly.kpxe", &[]).await.expect("served");
    assert_eq!(received, bytes);
    assert!(agreed.is_empty(), "nothing was asked for, so nothing was negotiated");
}

#[tokio::test]
async fn a_file_spanning_many_blocks_arrives_intact() {
    let served = Served::start("many").await;
    let bytes = payload(512 * 7 + 13);
    served.write("ipxe.efi", &bytes);

    let (received, _) = served.fetch("ipxe.efi", &[]).await.expect("served");
    assert_eq!(received.len(), bytes.len());
    assert_eq!(received, bytes);
}

#[tokio::test]
async fn a_file_that_is_an_exact_multiple_of_the_block_size_still_terminates() {
    // The classic TFTP bug. "Short block" is how the protocol says "that was
    // the last one", so a file whose length divides exactly needs an extra
    // empty block — without it the client waits for ever.
    let served = Served::start("exact").await;
    let bytes = payload(512 * 3);
    served.write("exact.bin", &bytes);

    let (received, _) = tokio::time::timeout(
        Duration::from_secs(10),
        served.fetch("exact.bin", &[]),
    )
    .await
    .expect("the transfer ended rather than hanging")
    .expect("served");

    assert_eq!(received, bytes);
}

#[tokio::test]
async fn an_empty_file_is_a_complete_transfer() {
    let served = Served::start("empty").await;
    served.write("empty.bin", b"");

    let (received, _) = served.fetch("empty.bin", &[]).await.expect("served");
    assert!(received.is_empty());
}

#[tokio::test]
async fn a_negotiated_block_size_is_honoured_exactly() {
    // An option acknowledged is an option that must then be obeyed: a client
    // told `blksize=1024` decides a block is the last one by its length.
    let served = Served::start("blksize").await;
    let bytes = payload(1024 * 2 + 1);
    served.write("big.bin", &bytes);

    let (received, agreed) =
        served.fetch("big.bin", &[("blksize", "1024")]).await.expect("served");

    assert_eq!(agreed.get("blksize").map(String::as_str), Some("1024"));
    assert_eq!(received, bytes);
}

#[tokio::test]
async fn a_block_size_past_what_fits_on_the_wire_is_granted_at_a_size_that_does() {
    // And the client is told which, so it is not waiting for packets that
    // would have to fragment.
    let served = Served::start("clamp").await;
    let bytes = payload(5000);
    served.write("big.bin", &bytes);

    let (received, agreed) =
        served.fetch("big.bin", &[("blksize", "65464")]).await.expect("served");

    assert_eq!(agreed.get("blksize").map(String::as_str), Some("1468"));
    assert_eq!(received, bytes);
}

#[tokio::test]
async fn the_size_is_reported_before_the_transfer_starts() {
    // `tsize` is how iPXE draws a progress bar.
    let served = Served::start("tsize").await;
    let bytes = payload(4242);
    served.write("sized.bin", &bytes);

    let (received, agreed) = served.fetch("sized.bin", &[("tsize", "0")]).await.expect("served");

    assert_eq!(agreed.get("tsize").map(String::as_str), Some("4242"));
    assert_eq!(received.len(), 4242);
}

#[tokio::test]
async fn a_window_of_blocks_is_sent_before_an_acknowledgement_is_waited_for() {
    // RFC 7440, and the difference between a 300MB image taking two minutes
    // and taking twenty.
    let served = Served::start("window").await;
    let bytes = payload(512 * 20 + 7);
    served.write("windowed.bin", &bytes);

    let (received, agreed) = served
        .fetch("windowed.bin", &[("windowsize", "8"), ("blksize", "512")])
        .await
        .expect("served");

    assert_eq!(agreed.get("windowsize").map(String::as_str), Some("8"));
    assert_eq!(received, bytes, "windowing must not reorder or drop anything");
}

#[tokio::test]
async fn a_file_that_is_not_there_is_an_error_naming_it() {
    let served = Served::start("missing").await;

    let (code, message) = served.fetch("nope.efi", &[]).await.expect_err("refused");
    assert_eq!(code, 1, "file not found");
    assert!(message.contains("nope.efi"), "{message}");
}

#[tokio::test]
async fn a_path_that_leaves_the_root_is_refused() {
    // The test that matters most: this listener is reachable by anything that
    // can send a UDP packet to the boot network.
    let served = Served::start("traversal").await;
    served.write("inside.bin", b"fine");

    for attempt in ["../../../etc/passwd", "..\\..\\windows\\system32\\config\\sam", ".env"] {
        let (code, _) = served.fetch(attempt, &[]).await.expect_err("refused");
        assert_eq!(code, 2, "`{attempt}` should be an access violation");
    }

    // And the directory itself still serves what is in it.
    assert_eq!(served.fetch("inside.bin", &[]).await.expect("served").0, b"fine");
}

#[tokio::test]
async fn a_write_is_refused_because_this_server_has_no_way_to_accept_one() {
    // TFTP has no authentication of any kind. A writable TFTP server on a boot
    // network is a way to replace the loader every machine is about to run.
    let served = Served::start("readonly").await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    let mut request = Vec::new();
    request.extend_from_slice(&2u16.to_be_bytes()); // WRQ
    request.extend_from_slice(b"evil.efi\0octet\0");
    client.send_to(&request, served.address).await.unwrap();

    let mut buffer = vec![0u8; 1024];
    let (length, _) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buffer))
        .await
        .expect("answered")
        .unwrap();

    match packet::decode(&buffer[..length]).unwrap() {
        Packet::Error { code, message } => {
            assert_eq!(code, 2, "access violation");
            assert!(message.contains("read-only"), "{message}");
        }
        other => panic!("a write should be refused, got {other:?}"),
    }
}

#[tokio::test]
async fn a_subdirectory_is_reachable_however_the_firmware_spells_the_path() {
    let served = Served::start("spelling").await;
    served.write("ipxe/undionly.kpxe", b"loader");

    for spelling in ["ipxe/undionly.kpxe", "/ipxe/undionly.kpxe", "\\ipxe\\undionly.kpxe"] {
        let (received, _) = served.fetch(spelling, &[]).await.expect("served");
        assert_eq!(received, b"loader", "{spelling}");
    }
}
