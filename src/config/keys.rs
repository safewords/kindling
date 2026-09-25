//! Typed configuration keys.
//!
//! The framework's own keys are re-exported at the bottom, so one import
//! reaches both `PXE_TFTP_ROOT` and `APP_NAME`.

use rainier_framework::config::config_keys;

config_keys! {
    /// The address machines should come back to. Everything a client is told
    /// to fetch is built from this, so getting it wrong is a rack that boots
    /// nothing — see `config/pxe.rs`, which refuses to guess it in production.
    pub PXE_SERVER_IP: String = "pxe.server_ip";
    /// The HTTP base for generated URLs, e.g. `http://10.0.0.2:8080`.
    pub PXE_HTTP_BASE: String = "pxe.http_base";
    /// What the PXE menu line says on a client's screen.
    pub PXE_DESCRIPTION: String = "pxe.description";
    /// The boot policy file.
    pub PXE_RULES_PATH: String = "pxe.rules_path";
    /// The directory TFTP serves and HTTP mirrors.
    pub PXE_TFTP_ROOT: String = "pxe.tftp_root";
    /// An optional IEEE OUI listing, for vendor names beyond the built-ins.
    pub PXE_OUI_FILE: String = "pxe.oui_file";

    pub PXE_DHCP_ENABLED: bool = "pxe.dhcp.enabled";
    pub PXE_DHCP_BIND: String = "pxe.dhcp.bind";
    /// Whether to also listen on 4011, the PXE boot server port.
    pub PXE_DHCP_BOOT_SERVER_PORT: bool = "pxe.dhcp.boot_server_port";

    pub PXE_TFTP_ENABLED: bool = "pxe.tftp.enabled";
    pub PXE_TFTP_BIND: String = "pxe.tftp.bind";
    pub PXE_TFTP_PORT: u16 = "pxe.tftp.port";
    pub PXE_TFTP_MAX_BLOCK_SIZE: u16 = "pxe.tftp.max_block_size";
    pub PXE_TFTP_MAX_WINDOW_SIZE: u16 = "pxe.tftp.max_window_size";

    /// The token that guards everything which changes state. Unset means the
    /// mutating endpoints refuse rather than run unguarded.
    pub PXE_API_TOKEN: String = "pxe.api_token";
    /// How many boot events to keep. Older ones are pruned nightly.
    pub PXE_EVENT_RETENTION_DAYS: i64 = "pxe.event_retention_days";

    /// Whole DHCP transactions for the same boot loader before a machine is
    /// treated as chainloading in a circle. Below 2 turns the check off.
    pub PXE_LOOP_THRESHOLD: usize = "pxe.loop.threshold";
    /// The window those transactions have to fall inside.
    pub PXE_LOOP_WINDOW_SECS: u64 = "pxe.loop.window_secs";
}

pub use rainier_framework::keys::*;

#[cfg(test)]
mod tests {
    use super::*;
    use rainier_framework::config::Config;

    #[test]
    fn the_applications_keys_and_the_frameworks_share_one_tree() {
        let config = Config::new();
        config.set(PXE_TFTP_PORT, 69u16).unwrap();
        config.set(APP_NAME, "kindling".to_string()).unwrap();

        assert_eq!(config.get(PXE_TFTP_PORT), Some(69));
        assert_eq!(config.get(APP_NAME).as_deref(), Some("kindling"));
    }

    #[test]
    fn this_applications_keys_stay_in_their_own_section() {
        for key in [PXE_SERVER_IP.path(), PXE_TFTP_ROOT.path(), PXE_API_TOKEN.path()] {
            assert!(key.starts_with("pxe."), "`{key}` is outside the application's section");
        }
    }
}
