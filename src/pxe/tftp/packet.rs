//! TFTP packets (RFC 1350) and the option extension (RFC 2347–2349, 7440).
//!
//! TFTP is a small protocol with a large number of ways to get it subtly
//! wrong, and most of them only show up as "the machine sits at a blinking
//! cursor". The three that matter here: a final block must be *short*, which
//! means a file whose length is an exact multiple of the block size needs an
//! extra empty one; block numbers are sixteen bits and wrap; and an option the
//! server acknowledges is an option the server must then honour exactly.

use std::collections::BTreeMap;

/// Opcodes.
pub const OP_RRQ: u16 = 1;
pub const OP_WRQ: u16 = 2;
pub const OP_DATA: u16 = 3;
pub const OP_ACK: u16 = 4;
pub const OP_ERROR: u16 = 5;
pub const OP_OACK: u16 = 6;

/// The block size every client understands without negotiating.
pub const DEFAULT_BLOCK_SIZE: u16 = 512;

/// Error codes from RFC 1350 and RFC 2347.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    NotDefined,
    FileNotFound,
    AccessViolation,
    DiskFull,
    IllegalOperation,
    UnknownTransferId,
    FileExists,
    NoSuchUser,
    OptionNegotiationFailed,
}

impl ErrorCode {
    pub fn as_u16(&self) -> u16 {
        match self {
            ErrorCode::NotDefined => 0,
            ErrorCode::FileNotFound => 1,
            ErrorCode::AccessViolation => 2,
            ErrorCode::DiskFull => 3,
            ErrorCode::IllegalOperation => 4,
            ErrorCode::UnknownTransferId => 5,
            ErrorCode::FileExists => 6,
            ErrorCode::NoSuchUser => 7,
            ErrorCode::OptionNegotiationFailed => 8,
        }
    }
}

/// A read request, with whatever options came with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    pub filename: String,
    pub mode: String,
    /// Option names are case-insensitive on the wire, so they are lowercased
    /// here and compared in one case everywhere else.
    pub options: BTreeMap<String, String>,
}

impl ReadRequest {
    pub fn option_u64(&self, name: &str) -> Option<u64> {
        self.options.get(name).and_then(|value| value.parse().ok())
    }

    /// Whether the client asked for `octet` — the only mode worth serving.
    ///
    /// `netascii` would line-ending-convert a kernel image, and `mail` has not
    /// existed since 1995.
    pub fn is_octet(&self) -> bool {
        self.mode.eq_ignore_ascii_case("octet")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Read(ReadRequest),
    Write { filename: String },
    Data { block: u16, data: Vec<u8> },
    Ack { block: u16 },
    Error { code: u16, message: String },
    Oack { options: BTreeMap<String, String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooShort,
    UnknownOpcode(u16),
    Malformed(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooShort => f.write_str("the datagram is too short to be TFTP"),
            DecodeError::UnknownOpcode(op) => write!(f, "opcode {op} is not TFTP"),
            DecodeError::Malformed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for DecodeError {}

pub fn decode(bytes: &[u8]) -> Result<Packet, DecodeError> {
    if bytes.len() < 2 {
        return Err(DecodeError::TooShort);
    }
    let opcode = u16::from_be_bytes([bytes[0], bytes[1]]);
    let rest = &bytes[2..];

    match opcode {
        OP_RRQ | OP_WRQ => {
            let mut fields = split_nul(rest);
            let filename = fields.next().ok_or(DecodeError::Malformed("no file name"))?;
            if opcode == OP_WRQ {
                return Ok(Packet::Write { filename });
            }
            let mode = fields.next().ok_or(DecodeError::Malformed("no transfer mode"))?;

            // Options come in name/value pairs. An odd trailing name with no
            // value is malformed, and dropping it is friendlier than refusing
            // the whole request.
            let mut options = BTreeMap::new();
            while let (Some(name), Some(value)) = (fields.next(), fields.next()) {
                options.insert(name.to_lowercase(), value);
            }

            Ok(Packet::Read(ReadRequest { filename, mode, options }))
        }
        OP_DATA => {
            if rest.len() < 2 {
                return Err(DecodeError::TooShort);
            }
            Ok(Packet::Data {
                block: u16::from_be_bytes([rest[0], rest[1]]),
                data: rest[2..].to_vec(),
            })
        }
        OP_ACK => {
            if rest.len() < 2 {
                return Err(DecodeError::TooShort);
            }
            Ok(Packet::Ack { block: u16::from_be_bytes([rest[0], rest[1]]) })
        }
        OP_ERROR => {
            if rest.len() < 2 {
                return Err(DecodeError::TooShort);
            }
            let message = split_nul(&rest[2..]).next().unwrap_or_default();
            Ok(Packet::Error { code: u16::from_be_bytes([rest[0], rest[1]]), message })
        }
        OP_OACK => {
            let mut fields = split_nul(rest);
            let mut options = BTreeMap::new();
            while let (Some(name), Some(value)) = (fields.next(), fields.next()) {
                options.insert(name.to_lowercase(), value);
            }
            Ok(Packet::Oack { options })
        }
        other => Err(DecodeError::UnknownOpcode(other)),
    }
}

/// NUL-terminated fields, decoded as UTF-8 with replacement.
///
/// Firmware sends path names in whatever encoding it has, and a lossy decode
/// of a name that will not resolve anyway beats refusing to parse the packet
/// and answering nothing at all.
fn split_nul(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).into_owned())
}

pub fn data(block: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&OP_DATA.to_be_bytes());
    out.extend_from_slice(&block.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn ack(block: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(4);
    out.extend_from_slice(&OP_ACK.to_be_bytes());
    out.extend_from_slice(&block.to_be_bytes());
    out
}

pub fn error(code: ErrorCode, message: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + message.len());
    out.extend_from_slice(&OP_ERROR.to_be_bytes());
    out.extend_from_slice(&code.as_u16().to_be_bytes());
    out.extend_from_slice(message.as_bytes());
    out.push(0);
    out
}

pub fn oack(options: &BTreeMap<String, String>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&OP_OACK.to_be_bytes());
    for (name, value) in options {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }
    out
}

/// Turn the name a client asked for into a relative path.
///
/// Firmware sends `\ipxe\undionly.kpxe` as readily as `ipxe/undionly.kpxe`,
/// and a leading slash is common. None of that changes which file is meant.
pub fn normalise_filename(filename: &str) -> String {
    let mut name = filename.replace('\\', "/");
    // Some routers (UniFi's "Network Boot") only accept a full `tftp://host/path`
    // as the boot file, and firmware then asks for that whole string by name.
    // The host is necessarily this server, so what matters is the path.
    if name.len() > 7 && name[..7].eq_ignore_ascii_case("tftp://") {
        name = name[7..].split_once('/').map(|(_, path)| path.to_string()).unwrap_or_default();
    }
    name.trim_start_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rrq(filename: &str, mode: &str, options: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&OP_RRQ.to_be_bytes());
        out.extend_from_slice(filename.as_bytes());
        out.push(0);
        out.extend_from_slice(mode.as_bytes());
        out.push(0);
        for (name, value) in options {
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(value.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn a_plain_read_request_decodes() {
        let Packet::Read(request) = decode(&rrq("undionly.kpxe", "octet", &[])).unwrap() else {
            panic!("a read request")
        };
        assert_eq!(request.filename, "undionly.kpxe");
        assert!(request.is_octet());
        assert!(request.options.is_empty());
    }

    #[test]
    fn options_decode_and_their_names_are_case_insensitive() {
        // RFC 2347 says the names are case-insensitive, and firmware takes
        // that literally: `blksize`, `BLKSIZE` and `BlkSize` all appear.
        let bytes = rrq("ipxe.efi", "OCTET", &[("BLKSIZE", "1468"), ("tsize", "0")]);
        let Packet::Read(request) = decode(&bytes).unwrap() else { panic!("a read request") };

        assert_eq!(request.option_u64("blksize"), Some(1468));
        assert_eq!(request.option_u64("tsize"), Some(0));
        assert!(request.is_octet(), "the mode is case-insensitive too");
    }

    #[test]
    fn a_trailing_option_with_no_value_is_dropped_rather_than_fatal() {
        let mut bytes = rrq("f", "octet", &[]);
        bytes.extend_from_slice(b"blksize\0");
        let Packet::Read(request) = decode(&bytes).unwrap() else { panic!("a read request") };
        assert!(request.options.is_empty());
    }

    #[test]
    fn a_truncated_packet_is_refused_rather_than_read_past() {
        assert_eq!(decode(&[]), Err(DecodeError::TooShort));
        assert_eq!(decode(&[0, 4]), Err(DecodeError::TooShort));
        assert_eq!(decode(&[0, 3, 0]), Err(DecodeError::TooShort));
        assert_eq!(decode(&[0, 9]), Err(DecodeError::UnknownOpcode(9)));
    }

    #[test]
    fn an_acknowledgement_decodes() {
        assert_eq!(decode(&ack(1)).unwrap(), Packet::Ack { block: 1 });
        assert_eq!(decode(&ack(65535)).unwrap(), Packet::Ack { block: 65535 });
    }

    #[test]
    fn a_data_packet_round_trips() {
        let encoded = data(7, b"hello");
        assert_eq!(decode(&encoded).unwrap(), Packet::Data { block: 7, data: b"hello".to_vec() });
    }

    #[test]
    fn an_error_carries_its_message() {
        let encoded = error(ErrorCode::FileNotFound, "no such file");
        assert_eq!(
            decode(&encoded).unwrap(),
            Packet::Error { code: 1, message: "no such file".into() }
        );
    }

    #[test]
    fn an_option_acknowledgement_round_trips() {
        let mut options = BTreeMap::new();
        options.insert("blksize".to_string(), "1468".to_string());
        options.insert("tsize".to_string(), "1048576".to_string());

        let Packet::Oack { options: decoded } = decode(&oack(&options)).unwrap() else {
            panic!("an OACK")
        };
        assert_eq!(decoded, options);
    }

    #[test]
    fn a_write_request_is_recognised_so_it_can_be_refused() {
        // This server is read-only, and the refusal needs to name the file.
        assert_eq!(
            decode(&{
                let mut out = Vec::new();
                out.extend_from_slice(&OP_WRQ.to_be_bytes());
                out.extend_from_slice(b"evil.bin\0octet\0");
                out
            })
            .unwrap(),
            Packet::Write { filename: "evil.bin".into() }
        );
    }

    #[test]
    fn firmware_spellings_of_a_path_all_mean_the_same_file() {
        for spelling in ["\\ipxe\\undionly.kpxe", "/ipxe/undionly.kpxe", "ipxe/undionly.kpxe"] {
            assert_eq!(normalise_filename(spelling), "ipxe/undionly.kpxe", "{spelling}");
        }
    }
}

#[cfg(test)]
mod url_filename_tests {
    use super::normalise_filename;

    #[test]
    fn a_full_tftp_url_as_the_file_name_means_its_path() {
        assert_eq!(normalise_filename("tftp://10.0.1.109/ipxe.efi"), "ipxe.efi");
        assert_eq!(normalise_filename("TFTP://boot.lan//images/a.efi"), "images/a.efi");
        assert_eq!(normalise_filename("tftp://10.0.1.109"), "");
        assert_eq!(normalise_filename("/ipxe.efi"), "ipxe.efi");
    }
}
