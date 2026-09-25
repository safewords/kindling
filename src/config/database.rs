//! Where the inventory lives.
//!
//! SQLite on disk by default, because a boot server's inventory is small, is
//! read far more than written, and should survive a restart on a machine
//! nobody has set a database up on. Point `DATABASE_URL` at Postgres or MySQL
//! and nothing else changes.

use rainier_framework::config::{Config, Env};
use rainier_framework::database::{DatabaseConfig, Databases};
use rainier_framework::prelude::*;

use crate::config::keys::{DATABASES, DATABASE_URL};

pub fn configure(config: &Config, env: &Env) -> Result<()> {
    if let Some(url) = env.get("DATABASE_URL").filter(|url| !url.trim().is_empty()) {
        // Parsed here so an unusable URL is a boot failure with a message,
        // rather than a connection error on the first machine that boots.
        Databases::from_url(&url)?;
        config.set(DATABASE_URL, url)?;
        return Ok(());
    }

    let path = env.string("DB_DATABASE", "storage/pxe.sqlite");
    config.set(DATABASE_URL, format!("sqlite://{path}?mode=rwc"))?;
    config.set(DATABASES, Databases::new("primary").with("primary", DatabaseConfig::sqlite(path)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inventory_survives_a_restart_by_default() {
        // An in-memory default would lose every machine the server had ever
        // seen, every time it was upgraded.
        let config = Config::new();
        configure(&config, &Env::parse("").isolated()).unwrap();
        let url = config.get(DATABASE_URL).unwrap();
        assert!(url.starts_with("sqlite://storage/pxe.sqlite"), "{url}");
        assert!(url.contains("mode=rwc"), "it has to be able to create the file: {url}");
    }

    #[test]
    fn a_database_url_wins_and_is_checked_before_the_server_starts() {
        let config = Config::new();
        configure(&config, &Env::parse("DATABASE_URL=sqlite::memory:").isolated()).unwrap();
        assert_eq!(config.get(DATABASE_URL).as_deref(), Some("sqlite::memory:"));
    }
}
