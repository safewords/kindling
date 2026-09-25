//! Option numbers, and the encapsulated PXE options inside option 43.
//!
//! Option 43 is the awkward one. It is a bag of sub-options in the same
//! `code, length, value` shape as the outer packet, and what goes in it
//! decides whether a machine downloads the boot file it was just offered or
//! spends fifteen seconds broadcasting for a boot server that is not there.
//! That fifteen seconds, times a rack, is why this module spells the bits out
//! rather than copying a magic constant from a forum post.

use std::net::Ipv4Addr;

/// The option numbers this server reads or writes.
pub mod code {
    pub const PAD: u8 = 0;
    pub const SUBNET_MASK: u8 = 1;
    pub const ROUTER: u8 = 3;
    pub const HOSTNAME: u8 = 12;
    pub const BOOT_FILE_SIZE: u8 = 13;
    pub const REQUESTED_IP: u8 = 50;
    pub const LEASE_TIME: u8 = 51;
    pub const OPTION_OVERLOAD: u8 = 52;
    pub const MESSAGE_TYPE: u8 = 53;
    pub const SERVER_IDENTIFIER: u8 = 54;
    pub const PARAMETER_REQUEST_LIST: u8 = 55;
    pub const MESSAGE: u8 = 56;
    pub const MAX_MESSAGE_SIZE: u8 = 57;
    pub const VENDOR_CLASS: u8 = 60;
    pub const CLIENT_IDENTIFIER: u8 = 61;
    pub const TFTP_SERVER_NAME: u8 = 66;
    pub const BOOTFILE_NAME: u8 = 67;
    /// RFC 4578: the Client System Architecture Type.
    pub const CLIENT_ARCH: u8 = 93;
    /// RFC 4578: the Client Network Interface Identifier (UNDI version).
    pub const CLIENT_NDI: u8 = 94;
    /// RFC 4578: the Client Machine Identifier — the SMBIOS UUID.
    pub const CLIENT_UUID: u8 = 97;
    /// RFC 3004: the User Class, where iPXE announces itself.
    pub const USER_CLASS: u8 = 77;
    /// iPXE's own encapsulated options.
    ///
    /// A second, independent signal that iPXE is the thing asking. It matters
    /// because the whole chainload chain ends on recognising iPXE, and a build
    /// configured without `DHCP_CLIENT_USER_CLASS`, or a relay that drops
    /// option 77, would otherwise be handed iPXE by iPXE for ever.
    pub const IPXE_ENCAP: u8 = 175;
    /// The encapsulated vendor options — for us, the PXE ones.
    pub const VENDOR_SPECIFIC: u8 = 43;
    /// RFC 3046: the relay agent's own information.
    pub const RELAY_AGENT: u8 = 82;
    pub const END: u8 = 255;
}

/// Sub-option numbers inside option 43, from the PXE 2.1 specification.
pub mod pxe {
    pub const MULTICAST_ADDRESS: u8 = 1;
    pub const DISCOVERY_CONTROL: u8 = 6;
    pub const MULTICAST_DISCOVERY_ADDRESS: u8 = 7;
    pub const BOOT_SERVERS: u8 = 8;
    pub const BOOT_MENU: u8 = 9;
    pub const MENU_PROMPT: u8 = 10;
    pub const BOOT_ITEM: u8 = 71;
    pub const END: u8 = 255;
}

/// The bits of PXE sub-option 6.
pub mod discovery {
    /// Do not broadcast looking for a boot server.
    pub const NO_BROADCAST: u8 = 0x01;
    /// Do not multicast looking for one either.
    pub const NO_MULTICAST: u8 = 0x02;
    /// Accept only the servers listed in sub-option 8.
    pub const ONLY_LISTED_SERVERS: u8 = 0x04;
    /// If the offer carried a boot file name, download it — no menu, no
    /// prompt, no discovery round trip.
    pub const USE_OFFERED_BOOTFILE: u8 = 0x08;

    /// What a proxy that already knows the answer sends: stop looking, the
    /// file you were offered is the file.
    ///
    /// The three bits together matter. `USE_OFFERED_BOOTFILE` alone leaves
    /// firmware free to go discovering first, and the discovery it does is a
    /// broadcast that times out.
    pub const ANSWER_IS_IN_THE_OFFER: u8 =
        NO_BROADCAST | NO_MULTICAST | USE_OFFERED_BOOTFILE;
}

/// The PXE server type in sub-options 8 and 9. Type 0 is "this bootstrap
/// server", which is what a proxy is.
pub const BOOTSERVER_TYPE_PXE: u16 = 0;

/// Build the contents of option 43 for a proxy answer.
///
/// `boot_servers` is normally just this server. The menu exists for firmware
/// that ignores the discovery control bits: one item, prompt timeout zero,
/// which auto-selects without showing anything.
pub fn proxy_vendor_options(
    description: &str,
    boot_servers: &[Ipv4Addr],
    discovery_control: u8,
) -> Vec<u8> {
    let mut out = Vec::new();

    push(&mut out, pxe::DISCOVERY_CONTROL, &[discovery_control]);

    if !boot_servers.is_empty() {
        let mut value = Vec::with_capacity(3 + boot_servers.len() * 4);
        value.extend_from_slice(&BOOTSERVER_TYPE_PXE.to_be_bytes());
        value.push(boot_servers.len() as u8);
        for server in boot_servers {
            value.extend_from_slice(&server.octets());
        }
        push(&mut out, pxe::BOOT_SERVERS, &value);
    }

    // A description longer than 255 bytes would overflow the length byte, and
    // it is a menu line nobody reads anyway.
    let description = truncate(description, 250);
    let mut menu = Vec::with_capacity(3 + description.len());
    menu.extend_from_slice(&BOOTSERVER_TYPE_PXE.to_be_bytes());
    menu.push(description.len() as u8);
    menu.extend_from_slice(description.as_bytes());
    push(&mut out, pxe::BOOT_MENU, &menu);

    // Timeout 0: take the first item immediately and display nothing. 255
    // would be "wait for a keypress", which in a rack is "wait".
    let mut prompt = Vec::with_capacity(1 + description.len());
    prompt.push(0);
    prompt.extend_from_slice(description.as_bytes());
    push(&mut out, pxe::MENU_PROMPT, &prompt);

    out.push(pxe::END);
    out
}

/// Build option 43 for a proxy answer to UEFI firmware: discovery control and
/// nothing else.
///
/// No menu, on purpose. EDK2 — which is most UEFI PXE, Hyper-V's and the Dell
/// PowerEdge 13th generation's included — treats a proxy offer with a boot
/// menu as a PXE 1.0 offer, auto-selects the first item, and reads item type 0
/// as "local boot": it aborts the network boot and starts DHCP over, for ever.
/// Without a menu the offer is a BINL-style one, and the firmware takes the
/// boot file it names (confirming on port 4011 first, which is answered).
pub fn uefi_proxy_vendor_options(discovery_control: u8) -> Vec<u8> {
    let mut out = Vec::new();
    push(&mut out, pxe::DISCOVERY_CONTROL, &[discovery_control]);
    out.push(pxe::END);
    out
}

/// Build option 43 for the boot server acknowledgement on port 4011, which
/// answers "which item did you pick" with "this one, layer zero".
pub fn boot_item_vendor_options() -> Vec<u8> {
    let mut out = Vec::new();
    let mut item = Vec::with_capacity(4);
    item.extend_from_slice(&BOOTSERVER_TYPE_PXE.to_be_bytes());
    item.extend_from_slice(&0u16.to_be_bytes()); // layer 0: the boot file itself
    push(&mut out, pxe::BOOT_ITEM, &item);
    out.push(pxe::END);
    out
}

fn push(out: &mut Vec<u8>, code: u8, value: &[u8]) {
    out.push(code);
    out.push(value.len() as u8);
    out.extend_from_slice(value);
}

/// Read the sub-options out of an option 43 value.
pub fn parse_vendor_options(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            0 => index += 1,
            pxe::END => break,
            code => {
                let Some(&length) = bytes.get(index + 1) else { break };
                let start = index + 2;
                let end = start + length as usize;
                if end > bytes.len() {
                    break;
                }
                out.push((code, bytes[start..end].to_vec()));
                index = end;
            }
        }
    }
    out
}

/// Decode an option's bytes as text.
///
/// Firmware sends strings with a trailing NUL about half the time, and a
/// vendor class with an invisible NUL on the end matches no glob anybody
/// writes.
pub fn to_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// The architecture from option 93, which is two bytes big-endian.
pub fn client_arch(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 2 {
        return None;
    }
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// The SMBIOS UUID from option 97, formatted the way everything else prints a
/// UUID.
///
/// The option is a one-byte type followed by sixteen bytes of UUID. Firmware
/// that sends the sixteen bytes without the type byte exists, so both lengths
/// are accepted.
pub fn client_uuid(bytes: &[u8]) -> Option<String> {
    let uuid: &[u8] = match bytes.len() {
        17 => &bytes[1..],
        16 => bytes,
        _ => return None,
    };

    let hex: String = uuid.iter().map(|byte| format!("{byte:02x}")).collect();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    // Not `&text[..max]`: a multi-byte character straddling the boundary would
    // panic.
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxy_options_tell_the_client_to_stop_looking() {
        // The bits that turn a fifteen-second discovery timeout into an
        // immediate download.
        let options = proxy_vendor_options("kindling", &["10.0.0.2".parse().unwrap()], discovery::ANSWER_IS_IN_THE_OFFER);
        let parsed = parse_vendor_options(&options);

        let control = parsed.iter().find(|(code, _)| *code == pxe::DISCOVERY_CONTROL).unwrap();
        assert_eq!(control.1, vec![0x0b]);
        assert_eq!(0x0b, discovery::NO_BROADCAST | discovery::NO_MULTICAST | discovery::USE_OFFERED_BOOTFILE);
    }

    #[test]
    fn the_boot_server_list_carries_this_server() {
        let options =
            proxy_vendor_options("kindling", &["10.0.0.2".parse().unwrap()], discovery::ANSWER_IS_IN_THE_OFFER);
        let parsed = parse_vendor_options(&options);

        let servers = parsed.iter().find(|(code, _)| *code == pxe::BOOT_SERVERS).unwrap();
        assert_eq!(servers.1, vec![0, 0, 1, 10, 0, 0, 2], "type 0, one server, its address");
    }

    #[test]
    fn the_menu_prompt_times_out_immediately_so_nothing_is_displayed() {
        // A prompt in a rack is a machine that waits forever.
        let options = proxy_vendor_options("kindling netboot", &[], discovery::ANSWER_IS_IN_THE_OFFER);
        let parsed = parse_vendor_options(&options);

        let (_, prompt) = parsed.iter().find(|(code, _)| *code == pxe::MENU_PROMPT).unwrap();
        assert_eq!(prompt[0], 0, "timeout 0 means auto-select");
        assert_eq!(to_string(&prompt[1..]), "kindling netboot");
    }

    #[test]
    fn a_description_longer_than_the_length_byte_is_cut_at_a_character_boundary() {
        // The cut is where a naive slice would panic on a multi-byte
        // character.
        let long = "é".repeat(200);
        let options = proxy_vendor_options(&long, &[], discovery::ANSWER_IS_IN_THE_OFFER);
        let parsed = parse_vendor_options(&options);
        let (_, menu) = parsed.iter().find(|(code, _)| *code == pxe::BOOT_MENU).unwrap();
        assert!(menu.len() <= 255);
    }

    #[test]
    fn the_boot_item_answer_names_layer_zero() {
        let parsed = parse_vendor_options(&boot_item_vendor_options());
        let (_, item) = parsed.iter().find(|(code, _)| *code == pxe::BOOT_ITEM).unwrap();
        assert_eq!(item, &vec![0, 0, 0, 0]);
    }

    #[test]
    fn a_trailing_nul_does_not_become_part_of_the_string() {
        // A vendor class with an invisible NUL on the end matches no glob
        // anybody writes.
        assert_eq!(to_string(b"PXEClient\0"), "PXEClient");
        assert_eq!(to_string(b"iPXE"), "iPXE");
        assert_eq!(to_string(b"  padded  "), "padded");
    }

    #[test]
    fn the_architecture_is_two_bytes_big_endian() {
        assert_eq!(client_arch(&[0x00, 0x07]), Some(7));
        assert_eq!(client_arch(&[0x00, 0x10]), Some(16));
        assert_eq!(client_arch(&[0x07]), None);
    }

    #[test]
    fn the_uuid_is_readable_with_or_without_its_type_byte() {
        let raw: Vec<u8> = (0..16).collect();
        let expected = "00010203-0405-0607-0809-0a0b0c0d0e0f";
        assert_eq!(client_uuid(&raw).as_deref(), Some(expected));

        let with_type: Vec<u8> = std::iter::once(0).chain(0..16).collect();
        assert_eq!(client_uuid(&with_type).as_deref(), Some(expected));

        assert_eq!(client_uuid(&[1, 2, 3]), None);
    }

    #[test]
    fn a_malformed_vendor_block_stops_rather_than_panicking() {
        assert_eq!(parse_vendor_options(&[6, 200, 1]), Vec::new());
        assert_eq!(parse_vendor_options(&[]), Vec::new());
        assert_eq!(parse_vendor_options(&[255]), Vec::new());
    }
}
