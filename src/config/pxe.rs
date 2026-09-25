//! Where this server is, and what it serves.
//!
//! One setting here is different from all the others: `PXE_SERVER_IP`. Every
//! URL a machine is told to fetch, and the `siaddr` in every DHCP reply, is
//! built from it — so a wrong value is not a degraded server, it is a rack
//! that downloads nothing and says nothing about why. It is detected when
//! nobody says, and the detection is logged so the answer is visible; in
//! production, with DHCP on, it must be stated.

use std::net::Ipv4Addr;

use rainier_framework::config::{Config, Env};
use rainier_framework::prelude::*;

use crate::config::keys::*;

pub fn configure(config: &Config, env: &Env) -> Result<()> {
    let dhcp_enabled = env.bool("PXE_DHCP_ENABLED", true);

    let declared = env.string("PXE_SERVER_IP", "");
    let server_ip = if declared.trim().is_empty() {
        let detected = detect_server_ip();

        // A server that guessed its own address in production would be found
        // out by a rack, at the worst possible time. Everywhere else the guess
        // is right often enough to be worth making, and it is logged either
        // way so the answer is never a mystery.
        //
        // Read from the environment rather than from `config`, and that is not
        // a style choice. This function runs twice — once against a bare tree
        // and once against the framework's, which by then has `app.env` in it.
        // Reading the tree made the two runs disagree: the first never
        // refused, so nothing propagated, and the second always did, into a
        // closure that can only log. The whole `pxe.*` section then quietly
        // failed to apply and the server came up pointed at 127.0.0.1.
        if dhcp_enabled && env.setting_or("APP_ENV", AppEnv::default())? == AppEnv::Production {
            return Err(Error::internal(format!(
                "PXE_SERVER_IP is not set. Every URL a machine is told to fetch is built from it, \
                 so this server will not guess in production. It looks like {detected} from here \
                 — set PXE_SERVER_IP to the address machines can reach this server on, or set \
                 PXE_DHCP_ENABLED=false if something else answers DHCP."
            )));
        }

        tracing::info!(
            address = %detected,
            "PXE_SERVER_IP is not set; using the address of the interface that reaches the \
             default route. Set it explicitly if that is the wrong one."
        );
        detected.to_string()
    } else {
        declared.parse::<Ipv4Addr>().map_err(|_| {
            Error::internal(format!(
                "PXE_SERVER_IP is `{declared}`, which is not an IPv4 address. It must be one, \
                 because it goes into the `siaddr` field of a DHCP reply, which is four bytes."
            ))
        })?;
        declared
    };

    let port = config.get(SERVER_PORT).unwrap_or(8080);
    let http_base = match env.get("PXE_HTTP_BASE").filter(|base| !base.trim().is_empty()) {
        Some(base) => base.trim_end_matches('/').to_string(),
        // Built from the same address everything else uses, so there is one
        // place to change when the server moves.
        None => format!("http://{server_ip}:{port}"),
    };

    config.set(PXE_SERVER_IP, server_ip)?;
    config.set(PXE_HTTP_BASE, http_base)?;
    config.set(PXE_DESCRIPTION, env.string("PXE_DESCRIPTION", "kindling netboot"))?;
    config.set(PXE_RULES_PATH, env.string("PXE_RULES_PATH", "pxe-rules.toml"))?;
    config.set(PXE_TFTP_ROOT, env.string("PXE_TFTP_ROOT", "tftproot"))?;
    config.set(PXE_OUI_FILE, env.string("PXE_OUI_FILE", ""))?;

    config.set(PXE_DHCP_ENABLED, dhcp_enabled)?;
    config.set(PXE_DHCP_BIND, env.string("PXE_DHCP_BIND", "0.0.0.0"))?;
    config.set(PXE_DHCP_BOOT_SERVER_PORT, env.bool("PXE_DHCP_BOOT_SERVER_PORT", true))?;

    config.set(PXE_TFTP_ENABLED, env.bool("PXE_TFTP_ENABLED", true))?;
    config.set(PXE_TFTP_BIND, env.string("PXE_TFTP_BIND", "0.0.0.0"))?;
    config.set(PXE_TFTP_PORT, env.int("PXE_TFTP_PORT", 69).clamp(1, 65535) as u16)?;

    // 1468 is a 1500-byte frame less the IP, UDP and TFTP headers. Above it
    // every packet fragments, which is slower than the smaller block.
    config.set(
        PXE_TFTP_MAX_BLOCK_SIZE,
        env.int("PXE_TFTP_MAX_BLOCK_SIZE", 1468).clamp(8, 65464) as u16,
    )?;
    config.set(
        PXE_TFTP_MAX_WINDOW_SIZE,
        env.int("PXE_TFTP_MAX_WINDOW_SIZE", 16).clamp(1, 64) as u16,
    )?;

    // A loop is four whole DHCP transactions for one loader inside ninety
    // seconds. A lap is five to ten seconds, so four of them fit comfortably,
    // while a machine that is merely slow never gets there. `0` turns it off.
    config.set(PXE_LOOP_THRESHOLD, env.int("PXE_LOOP_THRESHOLD", 4).max(0) as usize)?;
    config.set(PXE_LOOP_WINDOW_SECS, env.int("PXE_LOOP_WINDOW_SECS", 90).max(1) as u64)?;

    config.set(PXE_API_TOKEN, env.string("PXE_API_TOKEN", ""))?;
    config.set(
        PXE_EVENT_RETENTION_DAYS,
        env.int("PXE_EVENT_RETENTION_DAYS", 30).max(1),
    )?;

    Ok(())
}

/// The address of the interface that would reach the default route.
///
/// Connecting a UDP socket sends nothing — it only asks the routing table
/// which local address a packet to that destination would leave from. It is
/// the one portable way to answer "which of my addresses do other machines see
/// me as", and it works with no network at all, falling back to loopback.
pub fn detect_server_ip() -> Ipv4Addr {
    fn probe(destination: &str) -> Option<Ipv4Addr> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect(destination).ok()?;
        match socket.local_addr().ok()?.ip() {
            std::net::IpAddr::V4(v4) if !v4.is_unspecified() && !v4.is_loopback() => Some(v4),
            _ => None,
        }
    }

    // A public address first, then a private one: a host with no default route
    // but a LAN interface still answers correctly.
    probe("198.51.100.1:9")
        .or_else(|| probe("10.255.255.255:9"))
        .or_else(|| probe("192.168.255.255:9"))
        .unwrap_or(Ipv4Addr::LOCALHOST)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A developer's machine, which is what these defaults are about.
    /// Production has its own tests below, because unset means production and
    /// production is where this section refuses to guess.
    fn configured(env: &str) -> Result<Config> {
        let config = Config::new();
        config.set(SERVER_PORT, 8080u16)?;
        configure(&config, &Env::parse(&format!("APP_ENV=local
{env}")).isolated())?;
        Ok(config)
    }

    #[test]
    fn the_defaults_produce_a_server_that_runs_with_no_env_file_at_all() {
        let config = configured("").unwrap();
        assert_eq!(config.get(PXE_RULES_PATH).as_deref(), Some("pxe-rules.toml"));
        assert_eq!(config.get(PXE_TFTP_ROOT).as_deref(), Some("tftproot"));
        assert_eq!(config.get(PXE_TFTP_PORT), Some(69));
        assert_eq!(config.get(PXE_DHCP_ENABLED), Some(true));
        assert!(config.get(PXE_SERVER_IP).is_some(), "an address was detected");
    }

    #[test]
    fn the_http_base_is_built_from_the_server_address_and_port() {
        let config = configured("PXE_SERVER_IP=10.0.0.2").unwrap();
        assert_eq!(config.get(PXE_HTTP_BASE).as_deref(), Some("http://10.0.0.2:8080"));
    }

    #[test]
    fn a_reverse_proxy_can_name_the_base_itself() {
        // Behind TLS the machines reach a name, not this host's address.
        let config =
            configured("PXE_SERVER_IP=10.0.0.2\nPXE_HTTP_BASE=http://boot.example.internal/")
                .unwrap();
        assert_eq!(
            config.get(PXE_HTTP_BASE).as_deref(),
            Some("http://boot.example.internal"),
            "the trailing slash is removed once, here, rather than at every call site"
        );
    }

    #[test]
    fn an_address_that_is_not_an_address_stops_the_boot() {
        // It becomes four bytes in a DHCP reply. There is no "roughly".
        let error = configured("PXE_SERVER_IP=boot.example.com").unwrap_err();
        assert!(error.message().contains("PXE_SERVER_IP"), "{}", error.message());
        assert!(error.message().contains("four bytes"), "{}", error.message());
    }

    #[test]
    fn production_with_dhcp_on_refuses_to_guess_its_own_address() {
        let config = Config::new();
        config.set(SERVER_PORT, 8080u16).unwrap();

        // Unset means production.
        let error = configure(&config, &Env::parse("").isolated()).unwrap_err();
        assert!(error.message().contains("PXE_SERVER_IP is not set"), "{}", error.message());
        // And it says what it would have used, so the fix is a copy and paste.
        assert!(error.message().contains("It looks like"), "{}", error.message());
    }

    #[test]
    fn production_without_dhcp_does_not_need_one() {
        // Something else answers DHCP; this server only serves files and
        // scripts, and those URLs can come from PXE_HTTP_BASE.
        let config = Config::new();
        config.set(SERVER_PORT, 8080u16).unwrap();

        assert!(configure(&config, &Env::parse("PXE_DHCP_ENABLED=false").isolated()).is_ok());
    }

    #[test]
    fn the_environment_decides_this_and_not_the_tree_being_written_into() {
        // The regression that cost a whole section. This function runs twice
        // during a boot: once against a bare tree, and once against the
        // framework's, which by then holds `app.env`. Reading the *tree* made
        // those two runs disagree — the first never refused, so nothing
        // propagated, and the second always did, into a closure that can only
        // log. Every `pxe.*` setting then silently failed to apply and the
        // server came up telling machines to fetch from 127.0.0.1.
        let already_production = Config::new();
        already_production.set(APP_ENV, AppEnv::Production).unwrap();
        already_production.set(SERVER_PORT, 8080u16).unwrap();

        let env = Env::parse("APP_ENV=local").isolated();
        configure(&already_production, &env).expect("the environment says local, so it is local");

        assert!(
            already_production.get(PXE_SERVER_IP).is_some(),
            "the section applied, rather than half-applying"
        );
    }

    #[test]
    fn the_tftp_block_size_is_clamped_to_something_that_fits_on_a_wire() {
        let config = configured("PXE_TFTP_MAX_BLOCK_SIZE=999999").unwrap();
        assert_eq!(config.get(PXE_TFTP_MAX_BLOCK_SIZE), Some(65464));

        let config = configured("PXE_TFTP_MAX_BLOCK_SIZE=0").unwrap();
        assert_eq!(config.get(PXE_TFTP_MAX_BLOCK_SIZE), Some(8));
    }

    #[test]
    fn the_api_token_is_empty_rather_than_invented() {
        // An invented default would be a token in a log somewhere. Empty means
        // the mutating endpoints refuse, which is the safe half of the choice.
        let config = configured("").unwrap();
        assert_eq!(config.get(PXE_API_TOKEN).as_deref(), Some(""));
    }

    #[test]
    fn detection_always_answers_something_usable() {
        let detected = detect_server_ip();
        assert!(!detected.is_unspecified(), "0.0.0.0 is not an address to hand a client");
    }
}
