//! The HTTP listener.

use rainier_framework::config::{Config, Env};
use rainier_framework::prelude::*;

use crate::config::keys::{
    SERVER_COMPRESSION, SERVER_HOST, SERVER_MAX_BODY_BYTES, SERVER_PORT,
    SERVER_REQUEST_TIMEOUT_SECS,
};

pub fn configure(config: &Config, env: &Env) -> Result<()> {
    // Not 127.0.0.1. A boot server that only answers itself is not a boot
    // server, and the machines that need it are by definition elsewhere.
    config.set(SERVER_HOST, env.string("SERVER_HOST", "0.0.0.0"))?;
    config.set(SERVER_PORT, env.int("SERVER_PORT", 8080).clamp(1, 65535) as u16)?;

    // Images are served by streaming from disk, so nothing legitimate posts a
    // large body here: the API takes small JSON documents.
    config.set(SERVER_MAX_BODY_BYTES, env.int("SERVER_MAX_BODY", 256 * 1024).max(0) as u64)?;
    config.set(SERVER_REQUEST_TIMEOUT_SECS, env.int("SERVER_REQUEST_TIMEOUT", 30).max(0) as u64)?;
    config.set(SERVER_COMPRESSION, env.bool("SERVER_COMPRESSION", false))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_listens_where_the_machines_are() {
        let config = Config::new();
        configure(&config, &Env::parse("").isolated()).unwrap();
        assert_eq!(config.get(SERVER_HOST).as_deref(), Some("0.0.0.0"));
        assert_eq!(config.get(SERVER_PORT), Some(8080));
    }

    #[test]
    fn a_deployment_can_move_the_port() {
        let config = Config::new();
        configure(&config, &Env::parse("SERVER_PORT=80").isolated()).unwrap();
        assert_eq!(config.get(SERVER_PORT), Some(80));
    }
}
