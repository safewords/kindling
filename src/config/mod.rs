//! Configuration — one module per concern, all writing into the same dotted
//! tree. There is no `config/` directory of files to read; this is it.

use rainier_framework::config::{Config, Env};
use rainier_framework::prelude::*;

pub mod app;
pub mod database;
pub mod keys;
pub mod pxe;
pub mod server;

pub fn configure(config: &Config, env: &Env) -> Result<()> {
    app::configure(config, env)?;
    database::configure(config, env)?;
    // The server before the PXE section, because the HTTP base is built from
    // the port the server is about to listen on.
    server::configure(config, env)?;
    pxe::configure(config, env)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_is_reached() {
        let config = Config::new();
        configure(&config, &Env::parse("APP_ENV=local").isolated()).unwrap();

        for key in [
            keys::APP_NAME.path(),
            keys::DATABASE_URL.path(),
            keys::SERVER_PORT.path(),
            keys::PXE_SERVER_IP.path(),
            keys::PXE_TFTP_ROOT.path(),
            keys::PXE_RULES_PATH.path(),
        ] {
            assert!(config.has(key), "`{key}` was not set — is its section wired into configure?");
        }
    }

    #[test]
    fn the_http_base_follows_the_port_the_server_was_given() {
        // The ordering dependency this module's `configure` exists to get
        // right: the PXE section reads the port the server section just set.
        let config = Config::new();
        configure(&config, &Env::parse("SERVER_PORT=9090\nPXE_SERVER_IP=10.0.0.2").isolated())
            .unwrap();
        assert_eq!(config.get(keys::PXE_HTTP_BASE).as_deref(), Some("http://10.0.0.2:9090"));
    }
}
