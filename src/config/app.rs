//! The application's own identity.

use rainier_framework::config::{Config, Env};
use rainier_framework::prelude::*;

use crate::config::keys::{APP_NAME, APP_URL};

pub fn configure(config: &Config, env: &Env) -> Result<()> {
    config.set(APP_NAME, env.string("APP_NAME", "kindling"))?;
    config.set(APP_URL, env.string("APP_URL", "http://localhost:8080"))?;

    // Parsed once here so a misspelled APP_ENV stops the boot rather than
    // quietly meaning production — which is the value that turns several of
    // the framework's boot checks from warnings into refusals.
    let _ = env.setting::<AppEnv>("APP_ENV")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_apply() {
        let config = Config::new();
        configure(&config, &Env::parse("").isolated()).unwrap();
        assert_eq!(config.get(APP_NAME).as_deref(), Some("kindling"));
    }

    #[test]
    fn an_app_env_nobody_recognises_stops_the_boot() {
        let config = Config::new();
        let error = configure(&config, &Env::parse("APP_ENV=prodution").isolated()).unwrap_err();
        assert!(error.message().contains("APP_ENV"), "{}", error.message());
    }
}
