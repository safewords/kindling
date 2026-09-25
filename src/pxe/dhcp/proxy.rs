//! ProxyDHCP: answering the boot half of the conversation and nothing else.
//!
//! This server hands out no addresses. A network already has something doing
//! that — a router, a Windows server, somebody's `dnsmasq` — and standing up a
//! second DHCP server beside it is how a network stops working. What is
//! missing from that existing server is boot policy, so that is all this one
//! sends: `yiaddr` stays `0.0.0.0`, there is no lease, no netmask and no
//! gateway, and the reply carries a boot file and the PXE options that go with
//! it.
//!
//! A client that gets both replies merges them, which is what the PXE
//! specification describes and what every firmware in the field implements.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::net::UdpSocket;

use super::options::{
    boot_item_vendor_options, code, discovery, proxy_vendor_options, uefi_proxy_vendor_options,
};

/// Whether the client said it is UEFI firmware: option 93 present and not 0,
/// which is BIOS. A client that sends no option 93 is treated as BIOS, which
/// is what the PXE 2.1 ROMs that omit it are.
fn is_uefi(request: &DhcpPacket) -> bool {
    request
        .option(code::CLIENT_ARCH)
        .is_some_and(|arch| arch.len() >= 2 && u16::from_be_bytes([arch[0], arch[1]]) != 0)
}
use super::packet::{DhcpPacket, MessageType, Op, DEFAULT_MAX_MESSAGE};
use crate::pxe::arch::ClientArch;
use crate::pxe::facts::{ClientFacts, Stage};
use crate::pxe::oui::OuiDatabase;
use crate::pxe::policy::{FirmwareAnswer, ServerSettings};

/// The port the PXE specification puts a proxy's boot server on.
pub const BOOT_SERVER_PORT: u16 = 4011;
/// The port DHCP servers listen on.
pub const SERVER_PORT: u16 = 67;
/// The port clients listen on.
pub const CLIENT_PORT: u16 = 68;

/// What the server does with a packet, for the log and for the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Not ours: no PXE vendor class, or not a message we answer.
    NotOurs(&'static str),
    /// Ours, and policy said to stay quiet.
    Silent,
    /// Ours, and here is the reply.
    Reply(Box<Reply>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub packet: DhcpPacket,
    pub destination: SocketAddr,
    pub max: usize,
}

impl PartialEq for DhcpPacket {
    fn eq(&self, other: &Self) -> bool {
        self.encode(1472) == other.encode(1472)
    }
}

impl Eq for DhcpPacket {}

/// Read everything a rule might match on out of a DHCP packet.
pub fn facts_from_packet(packet: &DhcpPacket, source: SocketAddr, ouis: &OuiDatabase) -> Option<ClientFacts> {
    let mac = packet.client_mac()?;

    let arch = packet
        .option(code::CLIENT_ARCH)
        .and_then(|value| super::options::client_arch(&value))
        .map(ClientArch::from_code)
        // No option 93 at all means an old BIOS client: the option postdates
        // PXE and only BIOS firmware omits it.
        .unwrap_or(ClientArch::BIOS);

    let client_ip = if packet.ciaddr.is_unspecified() {
        match source.ip() {
            IpAddr::V4(v4) if !v4.is_unspecified() => Some(IpAddr::V4(v4)),
            _ => None,
        }
    } else {
        Some(IpAddr::V4(packet.ciaddr))
    };

    let relay = (!packet.giaddr.is_unspecified()).then_some(packet.giaddr);

    Some(
        ClientFacts::new(mac, arch, Stage::Firmware)
            .with_vendor_class(packet.option_string(code::VENDOR_CLASS))
            .with_user_class(packet.option(code::USER_CLASS).map(|value| user_class(&value)))
            .with_ipxe_options(packet.option(code::IPXE_ENCAP).is_some())
            .with_hostname(packet.option_string(code::HOSTNAME).filter(|name| !name.is_empty()))
            .with_uuid(packet.option(code::CLIENT_UUID).and_then(|value| super::options::client_uuid(&value)))
            .with_client_ip(client_ip)
            .with_relay_ip(relay)
            .with_transaction(Some(packet.xid))
            .identified(ouis),
    )
}

/// RFC 3004 wraps each user class in a length byte; iPXE and others just send
/// the bare string. Both spellings have to produce `iPXE`, because the whole
/// chainload chain hangs off recognising it.
fn user_class(bytes: &[u8]) -> String {
    if let Some((&length, rest)) = bytes.split_first() {
        if length as usize == rest.len() && length > 0 {
            return super::options::to_string(rest);
        }
    }
    super::options::to_string(bytes)
}

/// Whether this packet is a PXE client talking, rather than an ordinary DHCP
/// client that merely happens to be on the network.
///
/// The check is the vendor class. Answering something that is not asking to
/// network boot is how a proxy turns into a problem.
pub fn is_boot_request(packet: &DhcpPacket) -> bool {
    packet
        .option_string(code::VENDOR_CLASS)
        .map(|class| {
            let class = class.to_ascii_uppercase();
            class.starts_with("PXECLIENT") || class.starts_with("HTTPCLIENT")
        })
        .unwrap_or(false)
}

/// Decide what to send back.
///
/// Pure, so the whole matrix of message types and ports is testable without a
/// socket. `answer` is `None` when policy decided to stay quiet.
pub fn build_reply(
    request: &DhcpPacket,
    source: SocketAddr,
    port: u16,
    answer: Option<&FirmwareAnswer>,
    settings: &ServerSettings,
) -> Disposition {
    if request.op != Op::Request {
        return Disposition::NotOurs("not a BOOTREQUEST");
    }
    if !is_boot_request(request) {
        return Disposition::NotOurs("no PXEClient or HTTPClient vendor class");
    }

    let message_type = request.message_type();
    let reply_type = match (port, message_type) {
        // The first exchange: the client broadcasts a DISCOVER, and a proxy
        // answers with boot information and no address.
        (SERVER_PORT, Some(MessageType::Discover)) => MessageType::Offer,
        // A boot server request that arrived on 67 rather than 4011. Some
        // firmware broadcasts it; it is distinguishable from an address
        // request by carrying PXE vendor options.
        (SERVER_PORT, Some(MessageType::Request))
            if request.option(code::VENDOR_SPECIFIC).is_some() =>
        {
            MessageType::Ack
        }
        (SERVER_PORT, _) => {
            return Disposition::NotOurs("a DHCP exchange for an address, which is not ours");
        }
        // Port 4011 is the boot server port: everything arriving here is a
        // request for a boot file.
        (_, Some(MessageType::Request) | Some(MessageType::Inform)) => MessageType::Ack,
        (_, _) => return Disposition::NotOurs("not a boot server request"),
    };

    let Some(answer) = answer else {
        return Disposition::Silent;
    };

    let mut reply = DhcpPacket {
        op: Op::Reply,
        htype: request.htype,
        hlen: request.hlen,
        hops: 0,
        xid: request.xid,
        secs: 0,
        flags: request.flags,
        ciaddr: request.ciaddr,
        // Emphatically not an address: this server does not hand them out.
        yiaddr: Ipv4Addr::UNSPECIFIED,
        siaddr: settings.server_ip,
        giaddr: request.giaddr,
        chaddr: request.chaddr,
        sname: settings.server_ip.to_string().into_bytes(),
        file: Vec::new(),
        options: Vec::new(),
    };

    reply.set_option(code::MESSAGE_TYPE, vec![reply_type.as_byte()]);
    reply.set_option(code::SERVER_IDENTIFIER, settings.server_ip.octets().to_vec());

    // HTTP-boot firmware checks this string and refuses a reply that does not
    // carry its own class. Echoing the class the client used is the only thing
    // that works for both.
    let class = if answer.over_http { "HTTPClient" } else { "PXEClient" };
    reply.set_option_string(code::VENDOR_CLASS, class);

    // The PXE specification requires the machine identifier to be echoed.
    if let Some(uuid) = request.option(code::CLIENT_UUID) {
        reply.set_option(code::CLIENT_UUID, uuid);
    }

    if reply_type == MessageType::Ack && port == BOOT_SERVER_PORT {
        // "You asked which item; it is this one, layer zero."
        reply.set_option(code::VENDOR_SPECIFIC, boot_item_vendor_options());
    } else if is_uefi(request) {
        reply.set_option(
            code::VENDOR_SPECIFIC,
            uefi_proxy_vendor_options(discovery::ANSWER_IS_IN_THE_OFFER),
        );
    } else {
        reply.set_option(
            code::VENDOR_SPECIFIC,
            proxy_vendor_options(
                &settings.description,
                &[settings.server_ip],
                discovery::ANSWER_IS_IN_THE_OFFER,
            ),
        );
    }

    if !answer.over_http {
        reply.set_option_string(code::TFTP_SERVER_NAME, &settings.server_ip.to_string());
    }
    reply.set_option_string(code::BOOTFILE_NAME, &answer.file);

    // The `file` field is 128 bytes with a terminating NUL, and an HTTP URL
    // routinely exceeds that. Option 67 is authoritative and every PXE client
    // reads it; the field is filled in as well when it fits, for the older
    // firmware that only looks there.
    if answer.file.len() < 128 {
        reply.file = answer.file.clone().into_bytes();
    }

    Disposition::Reply(Box::new(Reply {
        destination: destination_for(request, source),
        max: request.max_message_size(),
        packet: reply,
    }))
}

/// Where the reply goes — RFC 2131 §4.1, with the proxy's own wrinkle.
///
/// A relay first, because a relayed request came from another segment and the
/// relay is the only way back. Then the client's own address if it has one.
/// Then, for a machine that has no address yet, whatever address the packet
/// came from — and if that is nothing either, the broadcast address, which is
/// the only thing left that reaches a machine mid-DISCOVER.
fn destination_for(request: &DhcpPacket, source: SocketAddr) -> SocketAddr {
    if !request.giaddr.is_unspecified() {
        return SocketAddr::new(IpAddr::V4(request.giaddr), SERVER_PORT);
    }
    if !request.ciaddr.is_unspecified() {
        return SocketAddr::new(IpAddr::V4(request.ciaddr), CLIENT_PORT);
    }
    if request.wants_broadcast() {
        return SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), CLIENT_PORT);
    }
    match source.ip() {
        IpAddr::V4(v4) if !v4.is_unspecified() => source,
        _ => SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), CLIENT_PORT),
    }
}

/// What the DHCP server asks of the application: given these facts, what file?
#[async_trait::async_trait]
pub trait BootPolicy: Send + Sync + 'static {
    async fn firmware_answer(&self, facts: ClientFacts) -> Option<FirmwareAnswer>;
}

/// The listener.
pub struct ProxyDhcpServer {
    settings: ServerSettings,
    policy: Arc<dyn BootPolicy>,
    ouis: Arc<OuiDatabase>,
}

impl ProxyDhcpServer {
    pub fn new(
        settings: ServerSettings,
        policy: Arc<dyn BootPolicy>,
        ouis: Arc<OuiDatabase>,
    ) -> Self {
        Self { settings, policy, ouis }
    }

    /// Take both ports, without serving on either yet.
    ///
    /// Both are bound before either is served, so a server that cannot have
    /// 4011 fails at startup rather than half-working — a machine whose
    /// firmware insists on the boot server request would otherwise hang, and
    /// the hang would be blamed on the machine.
    pub async fn bind(
        self,
        bind: Ipv4Addr,
        boot_server_port: bool,
    ) -> std::io::Result<BoundProxy> {
        let boot = boot_server_port
            .then(|| SocketAddr::new(IpAddr::V4(bind), BOOT_SERVER_PORT));

        self.bind_to(SocketAddr::new(IpAddr::V4(bind), SERVER_PORT), boot).await
    }

    /// Bind specific addresses rather than the well-known ports.
    ///
    /// The ports are fixed by the protocol, so this exists for the one case
    /// that is not a deployment: a test, which cannot take port 67 and should
    /// not need root to exercise the conversation.
    pub async fn bind_to(
        self,
        main: SocketAddr,
        boot: Option<SocketAddr>,
    ) -> std::io::Result<BoundProxy> {
        let main_socket = bind_udp(main).await?;
        let boot_socket = match boot {
            Some(address) => Some(bind_udp(address).await?),
            None => None,
        };

        Ok(BoundProxy { server: Arc::new(self), main: main_socket, boot: boot_socket })
    }

    /// Bind and serve until the process stops.
    pub async fn run(self, bind: Ipv4Addr, boot_server_port: bool) -> std::io::Result<()> {
        self.bind(bind, boot_server_port).await?.serve().await
    }

    async fn handle(&self, datagram: &[u8], source: SocketAddr, port: u16) -> Option<Reply> {
        let packet = match DhcpPacket::decode(datagram) {
            Ok(packet) => packet,
            Err(e) => {
                tracing::debug!(%source, error = %e, "ignoring a datagram that is not DHCP");
                return None;
            }
        };

        if !is_boot_request(&packet) {
            return None;
        }

        let facts = facts_from_packet(&packet, source, &self.ouis)?;
        let answer = self.policy.firmware_answer(facts.clone()).await;

        match build_reply(&packet, source, port, answer.as_ref(), &self.settings) {
            Disposition::Reply(reply) => Some(*reply),
            Disposition::Silent => {
                tracing::info!(mac = %facts.mac, "policy says to stay quiet for this machine");
                None
            }
            Disposition::NotOurs(why) => {
                tracing::trace!(mac = %facts.mac, why, "not answering");
                None
            }
        }
    }
}

/// A proxy holding its ports.
pub struct BoundProxy {
    server: Arc<ProxyDhcpServer>,
    main: UdpSocket,
    boot: Option<UdpSocket>,
}

impl BoundProxy {
    /// The address actually bound, which is how a test finds its port.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.main.local_addr()
    }

    pub async fn serve(self) -> std::io::Result<()> {
        tracing::info!(
            address = %self.main.local_addr().map(|a| a.to_string()).unwrap_or_default(),
            boot_server_port = self.boot.is_some(),
            server_ip = %self.server.settings.server_ip,
            "proxy DHCP listening — offering boot information, never addresses"
        );

        // Each socket is served knowing *which* of the two conversations it
        // carries, rather than reading it back off the port — so a test that
        // bound neither well-known port still exercises both paths.
        match self.boot {
            Some(boot) => {
                let (main, boot) = (Arc::new(self.main), Arc::new(boot));
                tokio::try_join!(
                    serve(Arc::clone(&self.server), main, SERVER_PORT),
                    serve(Arc::clone(&self.server), boot, BOOT_SERVER_PORT),
                )?;
                Ok(())
            }
            None => serve(self.server, Arc::new(self.main), SERVER_PORT).await,
        }
    }
}

async fn bind_udp(address: SocketAddr) -> std::io::Result<UdpSocket> {
    let socket = UdpSocket::bind(address).await.map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "could not bind {address}: {e}. Ports below 1024 need root (or \
                 CAP_NET_BIND_SERVICE); on Windows, an elevated prompt. If something else is \
                 already serving DHCP here, that is expected — this server is meant to run \
                 beside it, not on top of it."
            ),
        )
    })?;

    // A machine mid-DISCOVER has no address, so the only way to reach it is
    // the broadcast address — which a socket may not send to unless it says so
    // first.
    socket.set_broadcast(true)?;
    Ok(socket)
}

async fn serve(
    server: Arc<ProxyDhcpServer>,
    socket: Arc<UdpSocket>,
    port: u16,
) -> std::io::Result<()> {
    // The largest datagram worth reading. A DHCP packet that does not fit in
    // this is not one any firmware sends.
    let mut buffer = vec![0u8; 4096];

    loop {
        let (length, source) = match socket.recv_from(&mut buffer).await {
            Ok(received) => received,
            Err(e) => {
                // On Windows an ICMP port-unreachable for a previous send
                // surfaces as an error on the *receive*. Continuing is
                // correct: the socket is fine and the next client is
                // unaffected.
                tracing::warn!(error = %e, port, "recv failed; continuing");
                continue;
            }
        };

        let datagram = buffer[..length].to_vec();
        let server = Arc::clone(&server);
        let socket = Arc::clone(&socket);

        // Spawned, because the policy asks the database and a slow query must
        // not stall the machine next to it in the rack.
        tokio::spawn(async move {
            let Some(reply) = server.handle(&datagram, source, port).await else { return };

            let bytes = reply.packet.encode(reply.max.min(DEFAULT_MAX_MESSAGE.max(reply.max)));
            match socket.send_to(&bytes, reply.destination).await {
                Ok(sent) => tracing::debug!(
                    destination = %reply.destination,
                    bytes = sent,
                    "sent a proxy DHCP reply"
                ),
                Err(e) => tracing::warn!(
                    destination = %reply.destination,
                    error = %e,
                    "could not send the reply"
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ServerSettings {
        ServerSettings {
            server_ip: "10.0.0.2".parse().unwrap(),
            http_base: "http://10.0.0.2:8080".into(),
            description: "kindling netboot".into(),
        }
    }

    fn answer(file: &str, over_http: bool) -> FirmwareAnswer {
        FirmwareAnswer {
            file: file.into(),
            over_http,
            kind: crate::pxe::policy::AnswerKind::ChainLoader,
            reason: "test",
        }
    }

    /// Build a request the way firmware does.
    fn request(message: MessageType, vendor_class: Option<&str>) -> DhcpPacket {
        let mut packet = DhcpPacket {
            op: Op::Request,
            htype: 1,
            hlen: 6,
            xid: 0x1234,
            flags: 0x8000,
            ..Default::default()
        };
        packet.chaddr[..6].copy_from_slice(&[0x18, 0x66, 0xda, 0x11, 0x22, 0x33]);
        packet.set_option(code::MESSAGE_TYPE, vec![message.as_byte()]);
        packet.set_option(code::CLIENT_ARCH, vec![0x00, 0x07]);
        if let Some(class) = vendor_class {
            packet.set_option_string(code::VENDOR_CLASS, class);
        }
        packet
    }

    fn source() -> SocketAddr {
        "0.0.0.0:68".parse().unwrap()
    }

    #[test]
    fn a_discover_from_a_pxe_client_is_answered_with_an_offer_and_no_address() {
        // The defining property of a proxy: it must never look like it is
        // handing out an address, or two DHCP servers are fighting.
        let request = request(MessageType::Discover, Some("PXEClient:Arch:00007:UNDI:003001"));
        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer("ipxe.efi", false)), &settings())
        else {
            panic!("a PXE DISCOVER should be answered");
        };

        assert_eq!(reply.packet.message_type(), Some(MessageType::Offer));
        assert_eq!(reply.packet.yiaddr, Ipv4Addr::UNSPECIFIED, "no address is offered");
        assert_eq!(reply.packet.option(code::LEASE_TIME), None, "and no lease");
        assert_eq!(reply.packet.option(code::SUBNET_MASK), None);
        assert_eq!(reply.packet.siaddr, "10.0.0.2".parse::<Ipv4Addr>().unwrap());
        assert_eq!(reply.packet.xid, request.xid, "the transaction is the client's");
    }

    #[test]
    fn an_ordinary_dhcp_client_is_left_entirely_alone() {
        // No vendor class: this machine is asking for an address, not to boot.
        let no_class = request(MessageType::Discover, None);
        assert!(matches!(
            build_reply(&no_class, source(), SERVER_PORT, Some(&answer("ipxe.efi", false)), &settings()),
            Disposition::NotOurs(_)
        ));

        let laptop = request(MessageType::Discover, Some("MSFT 5.0"));
        assert!(matches!(
            build_reply(&laptop, source(), SERVER_PORT, Some(&answer("ipxe.efi", false)), &settings()),
            Disposition::NotOurs(_)
        ));
    }

    #[test]
    fn policy_saying_nothing_produces_no_packet_at_all() {
        let request = request(MessageType::Discover, Some("PXEClient"));
        assert_eq!(
            build_reply(&request, source(), SERVER_PORT, None, &settings()),
            Disposition::Silent
        );
    }

    #[test]
    fn the_boot_file_is_in_both_the_option_and_the_field_when_it_fits() {
        // Option 67 is authoritative; old firmware only reads the field.
        let request = request(MessageType::Discover, Some("PXEClient"));
        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer("ipxe/undionly.kpxe", false)), &settings())
        else {
            panic!("answered")
        };

        assert_eq!(
            reply.packet.option_string(code::BOOTFILE_NAME).as_deref(),
            Some("ipxe/undionly.kpxe")
        );
        assert_eq!(String::from_utf8(reply.packet.file.clone()).unwrap(), "ipxe/undionly.kpxe");
    }

    #[test]
    fn a_url_too_long_for_the_field_goes_in_the_option_only() {
        // The `file` field is 128 bytes. An HTTP boot URL routinely is not.
        let long = format!("http://10.0.0.2:8080/{}/boot.ipxe", "x".repeat(140));
        let request = request(MessageType::Discover, Some("HTTPClient:Arch:00016"));
        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer(&long, true)), &settings())
        else {
            panic!("answered")
        };

        assert_eq!(reply.packet.option_string(code::BOOTFILE_NAME).as_deref(), Some(long.as_str()));
        assert!(reply.packet.file.is_empty(), "truncating it into the field would be worse");
    }

    #[test]
    fn http_boot_firmware_gets_the_vendor_class_it_checks_for() {
        // UEFI HTTP boot refuses a reply whose option 60 is not HTTPClient.
        let request = request(MessageType::Discover, Some("HTTPClient:Arch:00016:UNDI:003001"));
        let Disposition::Reply(reply) = build_reply(
            &request,
            source(),
            SERVER_PORT,
            Some(&answer("http://10.0.0.2:8080/ipxe.efi", true)),
            &settings(),
        ) else {
            panic!("answered")
        };

        assert_eq!(reply.packet.option_string(code::VENDOR_CLASS).as_deref(), Some("HTTPClient"));
        assert_eq!(reply.packet.option(code::TFTP_SERVER_NAME), None, "there is no TFTP here");
    }

    #[test]
    fn tftp_boot_gets_pxeclient_and_a_tftp_server_name() {
        let request = request(MessageType::Discover, Some("PXEClient:Arch:00007"));
        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer("ipxe.efi", false)), &settings())
        else {
            panic!("answered")
        };

        assert_eq!(reply.packet.option_string(code::VENDOR_CLASS).as_deref(), Some("PXEClient"));
        assert_eq!(reply.packet.option_string(code::TFTP_SERVER_NAME).as_deref(), Some("10.0.0.2"));
    }

    #[test]
    fn the_machine_identifier_is_echoed_because_the_specification_says_so() {
        let mut request = request(MessageType::Discover, Some("PXEClient"));
        let uuid: Vec<u8> = std::iter::once(0).chain(0..16u8).collect();
        request.set_option(code::CLIENT_UUID, uuid.clone());

        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer("ipxe.efi", false)), &settings())
        else {
            panic!("answered")
        };
        assert_eq!(reply.packet.option(code::CLIENT_UUID), Some(uuid));
    }

    #[test]
    fn an_address_request_on_port_67_is_not_a_boot_request() {
        // A REQUEST with no PXE vendor options is the client accepting an
        // address from the real DHCP server. Answering it would be a second
        // server in the conversation.
        let request = request(MessageType::Request, Some("PXEClient"));
        assert!(matches!(
            build_reply(&request, source(), SERVER_PORT, Some(&answer("ipxe.efi", false)), &settings()),
            Disposition::NotOurs(_)
        ));
    }

    #[test]
    fn a_boot_server_request_broadcast_to_67_is_answered() {
        // Some firmware broadcasts the boot server request instead of
        // unicasting it to 4011. It is told apart by carrying PXE options.
        let mut request = request(MessageType::Request, Some("PXEClient"));
        request.set_option(code::VENDOR_SPECIFIC, vec![71, 4, 0, 0, 0, 0, 255]);

        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer("ipxe.efi", false)), &settings())
        else {
            panic!("a boot server request should be answered");
        };
        assert_eq!(reply.packet.message_type(), Some(MessageType::Ack));
    }

    #[test]
    fn port_4011_answers_the_boot_item_rather_than_the_menu() {
        // By this point the client has chosen; it is asking what item 0 is.
        let request = request(MessageType::Request, Some("PXEClient"));
        let Disposition::Reply(reply) = build_reply(
            &request,
            source(),
            BOOT_SERVER_PORT,
            Some(&answer("ipxe.efi", false)),
            &settings(),
        ) else {
            panic!("answered")
        };

        assert_eq!(reply.packet.message_type(), Some(MessageType::Ack));
        let vendor = reply.packet.option(code::VENDOR_SPECIFIC).unwrap();
        let parsed = super::super::options::parse_vendor_options(&vendor);
        assert!(parsed.iter().any(|(code, _)| *code == super::super::options::pxe::BOOT_ITEM));
    }

    #[test]
    fn uefi_firmware_is_offered_no_menu_because_edk2_reads_item_zero_as_local_boot() {
        use super::super::options::{parse_vendor_options, pxe};
        let offer_43 = |arch: u16| {
            let mut request = request(MessageType::Discover, Some("PXEClient"));
            request.set_option(code::CLIENT_ARCH, arch.to_be_bytes().to_vec());
            let Disposition::Reply(reply) = build_reply(
                &request,
                source(),
                SERVER_PORT,
                Some(&answer("ipxe.efi", false)),
                &settings(),
            ) else {
                panic!("answered")
            };
            parse_vendor_options(&reply.packet.option(code::VENDOR_SPECIFIC).unwrap())
                .into_iter()
                .map(|(code, _)| code)
                .collect::<Vec<_>>()
        };

        assert_eq!(offer_43(7), vec![pxe::DISCOVERY_CONTROL]);
        assert!(offer_43(0).contains(&pxe::BOOT_MENU));
    }

    #[test]
    fn an_inform_on_4011_is_answered_too() {
        let request = request(MessageType::Inform, Some("PXEClient"));
        assert!(matches!(
            build_reply(&request, source(), BOOT_SERVER_PORT, Some(&answer("f", false)), &settings()),
            Disposition::Reply(_)
        ));
    }

    #[test]
    fn a_relayed_request_is_answered_back_through_its_relay() {
        // The client is on another segment; the relay is the only way back.
        let mut request = request(MessageType::Discover, Some("PXEClient"));
        request.giaddr = "10.20.0.1".parse().unwrap();

        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer("f", false)), &settings())
        else {
            panic!("answered")
        };
        assert_eq!(reply.destination, "10.20.0.1:67".parse::<SocketAddr>().unwrap());
        assert_eq!(reply.packet.giaddr, request.giaddr, "the relay is echoed");
    }

    #[test]
    fn a_client_with_no_address_yet_is_answered_by_broadcast() {
        let request = request(MessageType::Discover, Some("PXEClient"));
        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer("f", false)), &settings())
        else {
            panic!("answered")
        };
        assert_eq!(reply.destination, "255.255.255.255:68".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn a_client_that_already_has_an_address_is_answered_directly() {
        let mut request = request(MessageType::Request, Some("PXEClient"));
        request.set_option(code::VENDOR_SPECIFIC, vec![255]);
        request.ciaddr = "10.0.0.50".parse().unwrap();

        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer("f", false)), &settings())
        else {
            panic!("answered")
        };
        assert_eq!(reply.destination, "10.0.0.50:68".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn the_facts_come_out_of_the_packet_intact() {
        let mut packet = request(MessageType::Discover, Some("PXEClient:Arch:00007:UNDI:003001"));
        packet.set_option_string(code::HOSTNAME, "lab-01");
        packet.set_option(code::USER_CLASS, b"\x04iPXE".to_vec());
        packet.set_option(code::CLIENT_UUID, std::iter::once(0).chain(0..16u8).collect::<Vec<_>>());

        let facts = facts_from_packet(&packet, source(), &OuiDatabase::new()).unwrap();
        assert_eq!(facts.mac.to_string(), "18:66:da:11:22:33");
        assert_eq!(facts.arch, ClientArch::X64_UEFI);
        assert_eq!(facts.vendor.as_deref(), Some("Dell"));
        assert_eq!(facts.hostname.as_deref(), Some("lab-01"));
        assert_eq!(facts.user_class.as_deref(), Some("iPXE"));
        assert!(facts.is_ipxe());
        assert_eq!(facts.uuid.as_deref(), Some("00010203-0405-0607-0809-0a0b0c0d0e0f"));
        assert_eq!(facts.stage, Stage::Firmware);
    }

    #[test]
    fn a_user_class_sent_either_way_round_reads_as_ipxe() {
        // RFC 3004 length-prefixes it; iPXE and others often do not. The whole
        // chainload chain hangs off recognising this string.
        let mut prefixed = request(MessageType::Discover, Some("PXEClient"));
        prefixed.set_option(code::USER_CLASS, b"\x04iPXE".to_vec());
        assert!(facts_from_packet(&prefixed, source(), &OuiDatabase::new()).unwrap().is_ipxe());

        let mut bare = request(MessageType::Discover, Some("PXEClient"));
        bare.set_option(code::USER_CLASS, b"iPXE".to_vec());
        assert!(facts_from_packet(&bare, source(), &OuiDatabase::new()).unwrap().is_ipxe());
    }

    #[test]
    fn ipxe_is_recognised_from_its_own_options_when_the_user_class_is_missing() {
        // A relay stripped option 77, or the build never set it. Option 175 is
        // only ever sent by iPXE, so it is enough on its own — and without it
        // this machine would be handed iPXE by iPXE, for ever.
        let mut packet = request(MessageType::Discover, Some("PXEClient"));
        packet.options.retain(|option| option.code != code::USER_CLASS);
        packet.set_option(code::IPXE_ENCAP, vec![0xb1, 1, 1, 255]);

        let facts = facts_from_packet(&packet, source(), &OuiDatabase::new()).unwrap();
        assert!(facts.user_class.is_none());
        assert!(facts.is_ipxe(), "recognised anyway");
    }

    #[test]
    fn a_client_that_sends_no_architecture_is_taken_for_bios() {
        // Option 93 postdates PXE; only BIOS firmware omits it.
        let mut packet = request(MessageType::Discover, Some("PXEClient"));
        packet.options.retain(|option| option.code != code::CLIENT_ARCH);

        let facts = facts_from_packet(&packet, source(), &OuiDatabase::new()).unwrap();
        assert_eq!(facts.arch, ClientArch::BIOS);
    }

    #[test]
    fn the_reply_fits_what_the_client_said_it_can_receive() {
        let mut request = request(MessageType::Discover, Some("PXEClient"));
        request.set_option(code::MAX_MESSAGE_SIZE, vec![0x01, 0x00]); // 256

        let Disposition::Reply(reply) =
            build_reply(&request, source(), SERVER_PORT, Some(&answer("ipxe.efi", false)), &settings())
        else {
            panic!("answered")
        };
        assert_eq!(reply.max, 256);
        assert!(reply.packet.encode(reply.max).len() <= 300, "padded to the BOOTP minimum, no more");
    }
}
