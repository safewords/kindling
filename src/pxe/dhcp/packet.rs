//! The BOOTP/DHCP packet, decoded and encoded.
//!
//! RFC 2131's fixed header followed by RFC 2132's options. The decoder is
//! deliberately forgiving about everything except length — firmware sends
//! short packets, long packets, options split across several instances and
//! options hidden in the `file` and `sname` fields — and deliberately strict
//! about never reading past the end of the buffer, because this is the one
//! place in the server that parses bytes from an unauthenticated stranger on
//! the local network.

use std::net::Ipv4Addr;

use super::options::{self, code};

/// The four bytes that separate a DHCP packet from a plain BOOTP one.
pub const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

/// The fixed header is 236 bytes, before the cookie.
const HEADER_LEN: usize = 236;

/// Anything shorter than this is not a DHCP packet.
const MIN_LEN: usize = HEADER_LEN + MAGIC_COOKIE.len();

/// BOOTP minimum. Some firmware silently drops a reply shorter than this, so
/// replies are padded up to it.
const BOOTP_MIN_REPLY: usize = 300;

/// The safe maximum when the client did not say (RFC 2131: 576 total, less 20
/// IP and 8 UDP header bytes).
pub const DEFAULT_MAX_MESSAGE: usize = 548;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Request,
    Reply,
    Other(u8),
}

impl Op {
    fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Op::Request,
            2 => Op::Reply,
            other => Op::Other(other),
        }
    }

    fn as_byte(&self) -> u8 {
        match self {
            Op::Request => 1,
            Op::Reply => 2,
            Op::Other(byte) => *byte,
        }
    }
}

/// DHCP option 53.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Discover,
    Offer,
    Request,
    Decline,
    Ack,
    Nak,
    Release,
    Inform,
    Other(u8),
}

impl MessageType {
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            1 => MessageType::Discover,
            2 => MessageType::Offer,
            3 => MessageType::Request,
            4 => MessageType::Decline,
            5 => MessageType::Ack,
            6 => MessageType::Nak,
            7 => MessageType::Release,
            8 => MessageType::Inform,
            other => MessageType::Other(other),
        }
    }

    pub fn as_byte(&self) -> u8 {
        match self {
            MessageType::Discover => 1,
            MessageType::Offer => 2,
            MessageType::Request => 3,
            MessageType::Decline => 4,
            MessageType::Ack => 5,
            MessageType::Nak => 6,
            MessageType::Release => 7,
            MessageType::Inform => 8,
            MessageType::Other(byte) => *byte,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            MessageType::Discover => "DISCOVER",
            MessageType::Offer => "OFFER",
            MessageType::Request => "REQUEST",
            MessageType::Decline => "DECLINE",
            MessageType::Ack => "ACK",
            MessageType::Nak => "NAK",
            MessageType::Release => "RELEASE",
            MessageType::Inform => "INFORM",
            MessageType::Other(_) => "UNKNOWN",
        }
    }
}

/// One option, as it appeared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpOption {
    pub code: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct DhcpPacket {
    pub op: Op,
    pub htype: u8,
    pub hlen: u8,
    pub hops: u8,
    pub xid: u32,
    pub secs: u16,
    pub flags: u16,
    pub ciaddr: Ipv4Addr,
    pub yiaddr: Ipv4Addr,
    pub siaddr: Ipv4Addr,
    pub giaddr: Ipv4Addr,
    pub chaddr: [u8; 16],
    pub sname: Vec<u8>,
    pub file: Vec<u8>,
    pub options: Vec<DhcpOption>,
}

impl Default for DhcpPacket {
    fn default() -> Self {
        Self {
            op: Op::Reply,
            htype: 1,
            hlen: 6,
            hops: 0,
            xid: 0,
            secs: 0,
            flags: 0,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr: Ipv4Addr::UNSPECIFIED,
            siaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: [0; 16],
            sname: Vec::new(),
            file: Vec::new(),
            options: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooShort(usize),
    NotDhcp,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooShort(len) => {
                write!(f, "{len} bytes is too short to be a DHCP packet")
            }
            DecodeError::NotDhcp => f.write_str("the DHCP magic cookie is missing"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl DhcpPacket {
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < MIN_LEN {
            return Err(DecodeError::TooShort(bytes.len()));
        }
        if bytes[HEADER_LEN..HEADER_LEN + 4] != MAGIC_COOKIE {
            return Err(DecodeError::NotDhcp);
        }

        let mut packet = DhcpPacket {
            op: Op::from_byte(bytes[0]),
            htype: bytes[1],
            hlen: bytes[2],
            hops: bytes[3],
            xid: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            secs: u16::from_be_bytes([bytes[8], bytes[9]]),
            flags: u16::from_be_bytes([bytes[10], bytes[11]]),
            ciaddr: ipv4(&bytes[12..16]),
            yiaddr: ipv4(&bytes[16..20]),
            siaddr: ipv4(&bytes[20..24]),
            giaddr: ipv4(&bytes[24..28]),
            chaddr: bytes[28..44].try_into().expect("16 bytes"),
            sname: trim_nul(&bytes[44..108]),
            file: trim_nul(&bytes[108..236]),
            options: Vec::new(),
        };

        packet.options = decode_options(&bytes[MIN_LEN..]);

        // RFC 2132 option 52: the `file` and `sname` fields may themselves
        // carry options when the option block ran out of room. Firmware that
        // does this is rare and the packets it produces are otherwise
        // undecodable, so it is worth the fifteen lines.
        if let Some(overload) = packet.option(code::OPTION_OVERLOAD).and_then(|v| v.first().copied())
        {
            let mut extra = Vec::new();
            if overload & 0x01 != 0 {
                extra.extend(decode_options(&bytes[108..236]));
                packet.file.clear();
            }
            if overload & 0x02 != 0 {
                extra.extend(decode_options(&bytes[44..108]));
                packet.sname.clear();
            }
            packet.options.extend(extra);
        }

        Ok(packet)
    }

    /// An option's value, with the RFC 3396 split-across-instances case put
    /// back together.
    pub fn option(&self, code: u8) -> Option<Vec<u8>> {
        let mut joined: Option<Vec<u8>> = None;
        for option in self.options.iter().filter(|option| option.code == code) {
            joined.get_or_insert_with(Vec::new).extend_from_slice(&option.data);
        }
        joined
    }

    pub fn option_string(&self, code: u8) -> Option<String> {
        self.option(code).map(|bytes| options::to_string(&bytes))
    }

    pub fn message_type(&self) -> Option<MessageType> {
        self.option(code::MESSAGE_TYPE)
            .and_then(|value| value.first().copied())
            .map(MessageType::from_byte)
    }

    /// The client's hardware address: `hlen` bytes of `chaddr`.
    pub fn client_mac(&self) -> Option<crate::pxe::mac::MacAddr> {
        if self.htype == 1 && self.hlen == 6 {
            crate::pxe::mac::MacAddr::from_bytes(&self.chaddr[..6])
        } else {
            // A non-Ethernet client, or one lying about its address length.
            // Some firmware also puts the address in option 61 instead.
            self.option(code::CLIENT_IDENTIFIER).and_then(|id| match id.split_first() {
                Some((1, rest)) => crate::pxe::mac::MacAddr::from_bytes(rest),
                _ => None,
            })
        }
    }

    pub fn wants_broadcast(&self) -> bool {
        self.flags & 0x8000 != 0
    }

    /// What the client said it can receive (option 57), clamped to something
    /// sane. A client asking for 64KB gets 1500-ish anyway because that is
    /// what fits in a frame without fragmenting.
    pub fn max_message_size(&self) -> usize {
        self.option(code::MAX_MESSAGE_SIZE)
            .filter(|value| value.len() >= 2)
            .map(|value| u16::from_be_bytes([value[0], value[1]]) as usize)
            .unwrap_or(DEFAULT_MAX_MESSAGE)
            .clamp(MIN_LEN, 1472)
    }

    /// The options the client asked for (option 55), in its order of
    /// preference.
    pub fn parameter_request_list(&self) -> Vec<u8> {
        self.option(code::PARAMETER_REQUEST_LIST).unwrap_or_default()
    }

    pub fn set_option(&mut self, code: u8, data: impl Into<Vec<u8>>) {
        let data = data.into();
        match self.options.iter_mut().find(|option| option.code == code) {
            Some(existing) => existing.data = data,
            None => self.options.push(DhcpOption { code, data }),
        }
    }

    pub fn set_option_string(&mut self, code: u8, value: &str) {
        self.set_option(code, value.as_bytes().to_vec());
    }

    /// Serialise, keeping the result under `max`.
    ///
    /// Options are written in the order they were set, and anything that does
    /// not fit is dropped rather than truncated — half an option is a packet
    /// the client cannot parse at all, which is worse than a missing one.
    pub fn encode(&self, max: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(BOOTP_MIN_REPLY.max(256));
        out.push(self.op.as_byte());
        out.push(self.htype);
        out.push(self.hlen);
        out.push(self.hops);
        out.extend_from_slice(&self.xid.to_be_bytes());
        out.extend_from_slice(&self.secs.to_be_bytes());
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(&self.ciaddr.octets());
        out.extend_from_slice(&self.yiaddr.octets());
        out.extend_from_slice(&self.siaddr.octets());
        out.extend_from_slice(&self.giaddr.octets());
        out.extend_from_slice(&self.chaddr);
        out.extend_from_slice(&fixed(&self.sname, 64));
        out.extend_from_slice(&fixed(&self.file, 128));
        out.extend_from_slice(&MAGIC_COOKIE);

        let max = max.max(MIN_LEN + 1);
        for option in &self.options {
            // Long options are split into 255-byte instances, which RFC 3396
            // says a client must rejoin.
            for chunk in option.data.chunks(255) {
                if out.len() + 2 + chunk.len() + 1 > max {
                    continue;
                }
                out.push(option.code);
                out.push(chunk.len() as u8);
                out.extend_from_slice(chunk);
            }
            if option.data.is_empty() && out.len() + 3 <= max {
                out.push(option.code);
                out.push(0);
            }
        }

        out.push(code::END);
        while out.len() < BOOTP_MIN_REPLY {
            out.push(0);
        }
        out
    }
}

fn decode_options(bytes: &[u8]) -> Vec<DhcpOption> {
    let mut options = Vec::new();
    let mut index = 0usize;

    while index < bytes.len() {
        match bytes[index] {
            code::PAD => index += 1,
            code::END => break,
            option_code => {
                let Some(&length) = bytes.get(index + 1) else { break };
                let start = index + 2;
                let end = start + length as usize;
                // A length that runs off the end of the buffer is where a
                // malformed packet becomes a panic. Stop instead: whatever was
                // decoded up to here is still usable.
                if end > bytes.len() {
                    break;
                }
                options.push(DhcpOption { code: option_code, data: bytes[start..end].to_vec() });
                index = end;
            }
        }
    }
    options
}

fn ipv4(bytes: &[u8]) -> Ipv4Addr {
    Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])
}

fn trim_nul(bytes: &[u8]) -> Vec<u8> {
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    bytes[..end].to_vec()
}

fn fixed(value: &[u8], width: usize) -> Vec<u8> {
    let mut out = vec![0u8; width];
    let take = value.len().min(width.saturating_sub(1));
    out[..take].copy_from_slice(&value[..take]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DISCOVER as a UEFI machine sends it.
    fn discover() -> Vec<u8> {
        let mut packet = vec![0u8; 236];
        packet[0] = 1; // BOOTREQUEST
        packet[1] = 1; // Ethernet
        packet[2] = 6; // 6-byte address
        packet[4..8].copy_from_slice(&0xdead_beefu32.to_be_bytes());
        packet[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
        packet[28..34].copy_from_slice(&[0x18, 0x66, 0xda, 0x11, 0x22, 0x33]);
        packet.extend_from_slice(&MAGIC_COOKIE);
        packet.extend_from_slice(&[code::MESSAGE_TYPE, 1, 1]);
        packet.extend_from_slice(&[code::CLIENT_ARCH, 2, 0x00, 0x07]);
        let vendor = b"PXEClient:Arch:00007:UNDI:003001";
        packet.push(code::VENDOR_CLASS);
        packet.push(vendor.len() as u8);
        packet.extend_from_slice(vendor);
        packet.extend_from_slice(&[code::END]);
        packet
    }

    #[test]
    fn a_discover_decodes_into_its_parts() {
        let packet = DhcpPacket::decode(&discover()).unwrap();
        assert_eq!(packet.op, Op::Request);
        assert_eq!(packet.xid, 0xdead_beef);
        assert!(packet.wants_broadcast());
        assert_eq!(packet.message_type(), Some(MessageType::Discover));
        assert_eq!(packet.client_mac().unwrap().to_string(), "18:66:da:11:22:33");
        assert_eq!(
            packet.option_string(code::VENDOR_CLASS).as_deref(),
            Some("PXEClient:Arch:00007:UNDI:003001")
        );
    }

    #[test]
    fn a_truncated_packet_is_refused_rather_than_read_past() {
        // The whole decoder exists on the far side of an unauthenticated UDP
        // socket, so this is the test that matters most.
        assert!(matches!(DhcpPacket::decode(&[]), Err(DecodeError::TooShort(0))));
        assert!(matches!(DhcpPacket::decode(&[0u8; 100]), Err(DecodeError::TooShort(100))));
    }

    #[test]
    fn something_that_is_not_dhcp_is_refused() {
        let mut bytes = vec![0u8; 240];
        bytes[236..240].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(DhcpPacket::decode(&bytes), Err(DecodeError::NotDhcp));
    }

    #[test]
    fn an_option_claiming_more_bytes_than_exist_stops_the_parse_without_panicking() {
        let mut bytes = discover();
        bytes.pop(); // drop the END
        bytes.extend_from_slice(&[60, 200, b'x']); // says 200 bytes, supplies 1

        let packet = DhcpPacket::decode(&bytes).expect("the header is still valid");
        assert_eq!(packet.message_type(), Some(MessageType::Discover), "earlier options survive");
    }

    #[test]
    fn an_option_split_across_instances_is_rejoined() {
        // RFC 3396. A client that splits option 43 and gets back only the
        // first half is a client that does not boot.
        let mut bytes = discover();
        bytes.pop();
        bytes.extend_from_slice(&[77, 3, b'i', b'P', b'X']);
        bytes.extend_from_slice(&[77, 1, b'E']);
        bytes.push(code::END);

        let packet = DhcpPacket::decode(&bytes).unwrap();
        assert_eq!(packet.option_string(77).as_deref(), Some("iPXE"));
    }

    #[test]
    fn a_reply_round_trips() {
        let mut reply = DhcpPacket { op: Op::Reply, xid: 42, ..Default::default() };
        reply.chaddr[..6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        reply.file = b"ipxe.efi".to_vec();
        reply.set_option(code::MESSAGE_TYPE, vec![MessageType::Offer.as_byte()]);
        reply.set_option_string(code::VENDOR_CLASS, "PXEClient");

        let encoded = reply.encode(DEFAULT_MAX_MESSAGE);
        let decoded = DhcpPacket::decode(&encoded).unwrap();

        assert_eq!(decoded.op, Op::Reply);
        assert_eq!(decoded.xid, 42);
        assert_eq!(decoded.message_type(), Some(MessageType::Offer));
        assert_eq!(String::from_utf8(decoded.file.clone()).unwrap(), "ipxe.efi");
        assert_eq!(decoded.option_string(code::VENDOR_CLASS).as_deref(), Some("PXEClient"));
    }

    #[test]
    fn a_reply_is_padded_to_the_bootp_minimum() {
        // Firmware exists that drops anything shorter, and the failure mode is
        // a machine that never boots and never says why.
        let reply = DhcpPacket::default();
        assert!(reply.encode(DEFAULT_MAX_MESSAGE).len() >= 300);
    }

    #[test]
    fn an_option_that_does_not_fit_is_dropped_whole() {
        // Half an option is a packet the client cannot parse at all.
        let mut reply = DhcpPacket::default();
        reply.set_option(code::MESSAGE_TYPE, vec![2]);
        reply.set_option(43, vec![0u8; 200]);

        let encoded = reply.encode(MIN_LEN + 10);
        let decoded = DhcpPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.message_type(), Some(MessageType::Offer));
        assert_eq!(decoded.option(43), None, "it was dropped rather than truncated");
    }

    #[test]
    fn a_long_option_is_split_into_instances_the_client_can_rejoin() {
        let mut reply = DhcpPacket::default();
        reply.set_option(43, vec![0xab; 300]);

        let encoded = reply.encode(1400);
        let decoded = DhcpPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.option(43).unwrap().len(), 300);
    }

    #[test]
    fn the_max_message_size_is_read_and_clamped() {
        let mut bytes = discover();
        bytes.pop();
        bytes.extend_from_slice(&[code::MAX_MESSAGE_SIZE, 2, 0xff, 0xff]);
        bytes.push(code::END);

        let packet = DhcpPacket::decode(&bytes).unwrap();
        assert_eq!(packet.max_message_size(), 1472, "a client asking for 64KB gets a frame");
    }

    #[test]
    fn a_client_with_no_chaddr_can_still_be_identified_by_option_61() {
        let mut bytes = vec![0u8; 236];
        bytes[0] = 1;
        bytes[1] = 0; // not Ethernet
        bytes[2] = 0;
        bytes.extend_from_slice(&MAGIC_COOKIE);
        bytes.extend_from_slice(&[code::MESSAGE_TYPE, 1, 1]);
        bytes.extend_from_slice(&[code::CLIENT_IDENTIFIER, 7, 1, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        bytes.push(code::END);

        let packet = DhcpPacket::decode(&bytes).unwrap();
        assert_eq!(packet.client_mac().unwrap().to_string(), "aa:bb:cc:dd:ee:ff");
    }

    #[test]
    fn options_hidden_in_the_file_field_are_found() {
        // Option 52 overloading. Rare, but a packet using it is otherwise
        // undecodable.
        let mut bytes = vec![0u8; 236];
        bytes[0] = 1;
        bytes[1] = 1;
        bytes[2] = 6;
        // The `file` field carries option 77.
        bytes[108] = 77;
        bytes[109] = 4;
        bytes[110..114].copy_from_slice(b"iPXE");
        bytes[114] = code::END;
        bytes.extend_from_slice(&MAGIC_COOKIE);
        bytes.extend_from_slice(&[code::MESSAGE_TYPE, 1, 1]);
        bytes.extend_from_slice(&[code::OPTION_OVERLOAD, 1, 1]);
        bytes.push(code::END);

        let packet = DhcpPacket::decode(&bytes).unwrap();
        assert_eq!(packet.option_string(77).as_deref(), Some("iPXE"));
        assert!(packet.file.is_empty(), "the field held options, not a file name");
    }
}
