//! Bootstrapping — one function that assembles the server.
//!
//! Configuration, the database, the provider that reads the boot policy,
//! middleware, routes, the schedule. Everything the rest of the application
//! relies on is wired here, in one readable place.

use std::sync::Arc;

use rainier_framework::config::{Config, Env};
use rainier_framework::database::Database;
use rainier_framework::observability::{MetricsSettings, TelemetrySettings};
use rainier_framework::prelude::*;
use rainier_framework::view::{TemplateEngine, Vite};

use crate::app::http::kernel;
use crate::app::providers::PxeServiceProvider;
use crate::app::services::LiveFeed;
use crate::config;
use crate::routes;

/// How the application is wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Normal operation.
    Running,
    /// Under test: an in-memory database, and nothing process-global.
    Testing,
}

pub async fn boot(mode: Mode) -> Result<Arc<Application>> {
    boot_with(mode, environment(mode)).await
}

/// The environment a mode runs on by default.
///
/// A test states its own premise: it reads no `.env` and nothing the shell
/// exported, because either would make the suite pass or fail depending on the
/// machine it ran on. `APP_ENV` is said out loud because unset means
/// *production*, which several checks reasonably refuse to guess in.
pub fn environment(mode: Mode) -> Env {
    match mode {
        Mode::Running => Env::load_or_default(".env"),
        Mode::Testing => {
            let mut env = Env::new();
            env.set("APP_ENV", "testing");

            // Each test boots against its own in-memory database, which the
            // starter policy is seeded into. Naming no import file keeps a
            // policy file lying in the working directory from deciding what
            // a test runs against.
            env.set("PXE_RULES_PATH", "");
            env.isolated()
        }
    }
}

/// Boot against an environment the caller built.
///
/// What this exists for: several things are read *once*, at boot — the API
/// token the write guard compares against, the paths the provider opens — so a
/// test that needs one of them different cannot set it afterwards. Handing the
/// environment in is the only honest way to say so.
pub async fn boot_with(mode: Mode, env: Env) -> Result<Arc<Application>> {

    // Read before the builder runs, because whether metrics exist decides
    // whether the middleware is installed — and middleware is declared, not
    // added later.
    let settings = Config::new();
    config::configure(&settings, &env)?;

    let database = connect(mode, &settings).await?;

    // What the admin interface watches. Made here because two things need it
    // before either exists: the provider, which hands it to everything that
    // writes, and the socket route, which is declared on the builder.
    let live = LiveFeed::new();

    let metrics = MetricsSettings::from_config(&settings);
    let telemetry = TelemetrySettings::from_config(&settings);
    let registry = metrics.registry();

    // Set if the builder's own configuration pass fails; checked after it.
    let configuration_error: Arc<std::sync::Mutex<Option<Error>>> = Arc::default();
    let carried = Arc::clone(&configuration_error);

    let mut builder = Rainier::new(".");
    if mode == Mode::Testing {
        // A test suite boots an application per test; installing a global
        // subscriber each time would have them fighting over process state.
        builder = builder.without_tracing();
    }
    if let Some(metrics) = registry.clone() {
        builder = builder.with_instance_arc(metrics);
    }

    let app = builder
        .configure(move |c| {
            // The builder's closure cannot return an error, so a failure is
            // carried out of it and raised below. Logging it and continuing
            // would leave the application running against a half-applied
            // configuration — which is how a boot server ends up telling a
            // whole rack to fetch everything from 127.0.0.1.
            if let Err(e) = config::configure(c, &env) {
                *configuration_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
            }

            // Said here rather than left to `APP_ENV`, because unset means
            // production — the right default for a deployment and the wrong
            // one for a test, several of whose boot checks refuse in
            // production where they would otherwise warn.
            if mode == Mode::Testing {
                let _ = c.set(config::keys::APP_ENV, AppEnv::Testing);
            }
        })
        // The shell template asks for the built bundle with `@vite`, so the
        // engine needs a resolver over `public` — where `npm run dev` writes
        // `hot` and `npm run build` writes `build/manifest.json`. An
        // application supplying its own engine attaches its own.
        .with_views(Arc::new(match mode {
            // Templates are re-read on every render outside production, so an
            // edit to an admin page shows up without a restart.
            Mode::Running => TemplateEngine::new("resources/views").with_vite(Vite::new("public")),
            Mode::Testing => TemplateEngine::new("resources/views")
                .without_cache()
                .with_vite(Vite::new("public").without_cache()),
        }))
        .with_database(database.clone())
        .with_provider(PxeServiceProvider { database, live: live.clone() })
        .with_websockets(routes::ws::routes(live))
        .with_schedule(routes::console::schedule)
        .with_middleware({
            let trace = telemetry.middleware();
            move |registry| kernel::register(registry, trace)
        })
        .with_routes(|router| {
            // Web first, so `/boot.ipxe` is matched before anything else could
            // claim it. Routes are tried in declaration order.
            routes::web::routes(router);
            routes::api::routes(router);
        })
        .boot()
        .await?;

    if let Some(e) = carried.lock().unwrap_or_else(|p| p.into_inner()).take() {
        return Err(e);
    }

    Ok(app)
}

/// Open the database.
///
/// SQLite on disk by default: a boot server's inventory is small, is read far
/// more than written, and has to survive a restart on a machine where nobody
/// has set up a database server. Point `DATABASE_URL` at Postgres or MySQL and
/// nothing else changes.
async fn connect(mode: Mode, settings: &Config) -> Result<Database> {
    use rainier_framework::drivers::sql::SeaOrmExecutor;
    use rainier_orm::PoolConfig;

    let url = match mode {
        Mode::Testing => "sqlite::memory:".to_string(),
        Mode::Running => settings.get_or(config::keys::DATABASE_URL, "sqlite::memory:".to_string()),
    };

    // An in-memory SQLite database exists only as long as the connection
    // holding it, so the pool must keep exactly one and never reap it.
    let pool = if url.starts_with("sqlite::memory:") || mode == Mode::Testing {
        PoolConfig::in_memory()
    } else {
        PoolConfig::default()
    };

    // Make sure the directory for a file-backed database exists. The
    // alternative is `unable to open database file`, which is a true statement
    // that tells nobody the parent directory is missing.
    if let Some(path) = sqlite_path(&url) {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Error::internal(format!(
                        "could not create `{}` for the inventory database: {e}",
                        parent.display()
                    ))
                })?;
            }
        }
    }

    let executor = SeaOrmExecutor::connect(&url, &pool)
        .await
        .map_err(|e| Error::internal(format!("could not connect to `{}`: {e}", redact(&url))))?;

    Ok(Database::new(executor))
}

/// The file a `sqlite://…` URL points at, if it points at one.
fn sqlite_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("sqlite://")?;
    if rest.starts_with(':') {
        return None;
    }
    Some(rest.split('?').next().unwrap_or(rest).to_string())
}

/// A database URL with its password removed, for the connection error.
///
/// The URL carries the password, so printing it verbatim would put a live
/// credential in every crash log of a server whose database is down — which is
/// exactly the situation that crashloops.
fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    // The LAST `@` ends the userinfo: a password holding a raw `@` would
    // otherwise split early and print its tail as part of the host.
    let Some((userinfo, host)) = rest.rsplit_once('@') else {
        return url.to_string();
    };
    // The FIRST `:` ends the user, so a password holding a `:` does not leak
    // its tail either.
    let user = userinfo.split_once(':').map_or(userinfo, |(user, _)| user);
    format!("{scheme}://{user}:***@{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_failure_does_not_log_the_password() {
        assert_eq!(
            redact("mysql://pxe:hunter2@db.internal:3306/pxe"),
            "mysql://pxe:***@db.internal:3306/pxe"
        );
        assert_eq!(redact("mysql://user:pa:ss@host/db"), "mysql://user:***@host/db");

        let at_sign = redact("mysql://user:p@ss@host/db");
        assert_eq!(at_sign, "mysql://user:***@host/db");
        assert!(!at_sign.contains("ss@"), "the tail of the password leaked: {at_sign}");
    }

    #[test]
    fn a_url_with_no_credentials_is_left_alone() {
        for url in ["sqlite::memory:", "sqlite://storage/pxe.sqlite?mode=rwc"] {
            assert_eq!(redact(url), url);
        }
    }

    #[test]
    fn the_directory_for_a_file_database_is_found_in_its_url() {
        assert_eq!(
            sqlite_path("sqlite://storage/pxe.sqlite?mode=rwc").as_deref(),
            Some("storage/pxe.sqlite")
        );
        assert_eq!(sqlite_path("sqlite::memory:"), None, "there is no file to make room for");
        assert_eq!(sqlite_path("postgres://host/db"), None);
    }

    #[tokio::test]
    async fn the_application_boots_under_test() {
        // The whole wiring, exercised: configuration, the database, the
        // provider that loads the policy from the database, the routes.
        let app = boot(Mode::Testing).await.expect("the application boots");

        assert!(app.resolve::<crate::app::services::BootService>().is_ok());
        assert!(app.resolve::<crate::app::services::RuleStore>().is_ok());
        assert!(app.resolve::<crate::app::repositories::HostRepository>().is_ok());
    }
}
