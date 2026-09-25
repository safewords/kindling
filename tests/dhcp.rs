//! Proxy DHCP over a real socket.
//!
//! The reply matrix is unit-tested next to the code. What is tested here is
//! the wiring between the socket and the policy: a real packet in, facts read
//! off it, a decision made against a real rule set, and a real packet back —
//! on ephemeral ports, so it needs no privileges.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use pxe::pxe::arch::ClientArch;
use pxe::pxe::dhcp::options::{code, pxe as sub, parse_vendor_options};
use pxe::pxe::dhcp::packet::{DhcpPacket, MessageType, Op, MAGIC_COOKIE};
use pxe::pxe::dhcp::proxy::{BootPolicy, ProxyDhcpServer};
use pxe::pxe::facts::ClientFacts;
use pxe::pxe::oui::OuiDatabase;
use pxe::pxe::policy::{self, FirmwareAnswer, Overrides, ServerSettings};
use pxe::pxe::rules::RuleSet;
use tokio::net::UdpSocket;

const RULES: &str = r#"
[settings]
default_profile = "local"

[bootloaders]
bios = "undionly.kpxe"
x64-uefi = "ipxe.efi"

[profiles.local]
kind = "local"

[profiles.hands-off]
kind = "ignore"

[[rule]]
name = "leave-the-switches-alone"
when = { device_class = ["network"] }
profile = "hands-off"
"#;

/// The real policy path, against a rule set this test owns.
struct Policy {
    rules: RuleSet,
    settings: ServerSettings,
}

#[async_trait::async_trait]
impl BootPolicy for Policy {
    async fn firmware_answer(&self, facts: ClientFacts) -> Option<FirmwareAnswer> {
        let decision = policy::decide(&self.rules, &facts, &Overrides::default());
        policy::firmware_answer(&decision, &facts, &self.rules, &self.settings)
    }
}

fn settings() -> ServerSettings {
    ServerSettings {
        server_ip: Ipv4Addr::new(127, 0, 0, 1),
        http_base: "http://127.0.0.1:8080".into(),
        description: "kindling netboot".into(),
    }
}

/// Start a proxy on ephemeral ports and hand back the one to talk to.
async fn start() -> SocketAddr {
    let policy =
        Policy { rules: RuleSet::parse(RULES).expect("valid rules"), settings: settings() };

    let bound = ProxyDhcpServer::new(settings(), Arc::new(policy), Arc::new(OuiDatabase::new()))
        .bind_to("127.0.0.1:0".parse().unwrap(), None)
        .await
        .expect("an ephemeral port");

    let address = bound.local_addr().expect("the port it took");
    tokio::spawn(async move { bound.serve().await });
    address
}

/// A DISCOVER as firmware sends it — but with the broadcast flag clear, so the
/// reply comes back to this socket rather than to the whole segment.
fn discover(mac: [u8; 6], arch: u16, vendor_class: &str) -> Vec<u8> {
    let mut packet = DhcpPacket {
        op: Op::Request,
        htype: 1,
        hlen: 6,
        xid: 0x5afe_1234,
        flags: 0,
        ..Default::default()
    };
    packet.chaddr[..6].copy_from_slice(&mac);
    packet.set_option(code::MESSAGE_TYPE, vec![MessageType::Discover.as_byte()]);
    packet.set_option(code::CLIENT_ARCH, arch.to_be_bytes().to_vec());
    packet.set_option_string(code::VENDOR_CLASS, vendor_class);
    packet.encode(1400)
}

async fn ask(server: SocketAddr, datagram: &[u8]) -> Option<DhcpPacket> {
    let client = UdpSocket::bind("127.0.0.1:0").await.expect("a client socket");
    client.send_to(datagram, server).await.expect("sent");

    let mut buffer = vec![0u8; 2048];
    match tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buffer)).await {
        Ok(Ok((length, _))) => Some(DhcpPacket::decode(&buffer[..length]).expect("a DHCP reply")),
        // A timeout is a real answer here: staying quiet is something this
        // server does deliberately.
        _ => None,
    }
}

#[tokio::test]
async fn a_bios_machine_is_offered_the_bios_loader() {
    let server = start().await;

    let reply = ask(server, &discover([0x18, 0x66, 0xda, 1, 2, 3], 0, "PXEClient:Arch:00000"))
        .await
        .expect("a PXE client is answered");

    assert_eq!(reply.op, Op::Reply);
    assert_eq!(reply.message_type(), Some(MessageType::Offer));
    assert_eq!(reply.option_string(code::BOOTFILE_NAME).as_deref(), Some("undionly.kpxe"));
    assert_eq!(reply.xid, 0x5afe_1234, "the client's own transaction");
}

#[tokio::test]
async fn a_uefi_machine_is_offered_the_uefi_loader() {
    // The whole reason option 93 is read: the other binary does not run, and
    // the machine says nothing about why.
    let server = start().await;

    let reply = ask(
        server,
        &discover([0x18, 0x66, 0xda, 1, 2, 3], ClientArch::X64_UEFI.code(), "PXEClient:Arch:00007"),
    )
    .await
    .expect("answered");

    assert_eq!(reply.option_string(code::BOOTFILE_NAME).as_deref(), Some("ipxe.efi"));
}

#[tokio::test]
async fn the_offer_carries_no_address_and_no_lease() {
    // The defining property of a proxy. Anything else here is a second DHCP
    // server fighting the real one.
    let server = start().await;

    let reply = ask(server, &discover([0x18, 0x66, 0xda, 1, 2, 3], 0, "PXEClient"))
        .await
        .expect("answered");

    assert_eq!(reply.yiaddr, Ipv4Addr::UNSPECIFIED, "no address is offered");
    assert_eq!(reply.option(code::LEASE_TIME), None);
    assert_eq!(reply.option(code::SUBNET_MASK), None);
    assert_eq!(reply.option(code::ROUTER), None);
}

#[tokio::test]
async fn the_offer_tells_the_client_to_stop_looking_for_a_boot_server() {
    // Without these bits the firmware broadcasts for a boot server and waits
    // for the timeout — fifteen seconds, times a rack.
    let server = start().await;

    let reply = ask(server, &discover([0x18, 0x66, 0xda, 1, 2, 3], 0, "PXEClient"))
        .await
        .expect("answered");

    let vendor = reply.option(code::VENDOR_SPECIFIC).expect("PXE options");
    let parsed = parse_vendor_options(&vendor);

    let (_, control) = parsed
        .iter()
        .find(|(code, _)| *code == sub::DISCOVERY_CONTROL)
        .expect("a discovery control sub-option");

    assert_eq!(control[0] & 0x01, 0x01, "no broadcast discovery");
    assert_eq!(control[0] & 0x02, 0x02, "no multicast discovery");
    assert_eq!(control[0] & 0x08, 0x08, "use the file in this offer");
}

#[tokio::test]
async fn an_ordinary_dhcp_client_gets_no_reply_at_all() {
    // A laptop asking for an address is not this server's business, and
    // answering it is how a proxy becomes a network problem.
    let server = start().await;

    let reply = ask(server, &discover([0xaa, 0xbb, 0xcc, 1, 2, 3], 0, "MSFT 5.0")).await;
    assert!(reply.is_none(), "a non-PXE client should be left alone");
}

#[tokio::test]
async fn a_machine_the_policy_ignores_gets_no_reply_at_all() {
    // `kind = "ignore"`, reached through the real rule engine: a Cisco OUI.
    let server = start().await;

    let reply = ask(server, &discover([0x00, 0x00, 0x0c, 1, 2, 3], 0, "PXEClient")).await;
    assert!(reply.is_none(), "the policy said to stay quiet");
}

#[tokio::test]
async fn a_datagram_that_is_not_dhcp_is_ignored_without_crashing_the_listener() {
    // The listener is reachable by anything on the network, so it has to
    // survive nonsense and keep serving.
    let server = start().await;

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for junk in [vec![0u8; 3], vec![0xff; 600], b"hello there".to_vec()] {
        client.send_to(&junk, server).await.unwrap();
    }

    // Still answering afterwards.
    let reply = ask(server, &discover([0x18, 0x66, 0xda, 1, 2, 3], 0, "PXEClient"))
        .await
        .expect("the listener survived");
    assert_eq!(reply.message_type(), Some(MessageType::Offer));
}

#[tokio::test]
async fn a_packet_with_an_option_longer_than_the_buffer_does_not_take_the_server_down() {
    let server = start().await;

    let mut malformed = discover([0x18, 0x66, 0xda, 1, 2, 3], 0, "PXEClient");
    // Truncate mid-option: the last option now claims more bytes than remain.
    malformed.truncate(MAGIC_COOKIE.len() + 240);
    malformed.extend_from_slice(&[60, 200, b'x']);

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(&malformed, server).await.unwrap();

    let reply = ask(server, &discover([0x18, 0x66, 0xda, 1, 2, 3], 0, "PXEClient"))
        .await
        .expect("the listener survived");
    assert_eq!(reply.message_type(), Some(MessageType::Offer));
}
