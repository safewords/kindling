//! Everything this server is made of, bound in one place.
//!
//! The boot policy and the vendor list are read **here, eagerly**, rather than
//! behind a lazy singleton. That is the whole point of the provider: a policy
//! that will not load should stop the boot at a terminal somebody is looking
//! at, not the first time a machine asks a question at 4am. The policy lives in
//! the database, so it is read in the async half, right after the migrations
//! that create its tables.

use std::sync::Arc;

use rainier_framework::config::Config;
use rainier_framework::prelude::*;
use rainier_framework::public::PublicFiles;

use crate::app::repositories::{BootEventRepository, HostRepository, PolicyRepository};
use crate::app::services::{BootService, LiveFeed, LoopBreaker, RuleStore};
use crate::config::keys::*;
use crate::pxe::oui::OuiDatabase;
use crate::pxe::policy::ServerSettings;

pub struct PxeServiceProvider {
    pub database: Database,
    /// Made before the provider runs, because the WebSocket route that reads
    /// it is declared on the builder and the builder comes first. Bound here
    /// with everything that writes to it.
    pub live: LiveFeed,
}

impl ServiceProvider for PxeServiceProvider {
    fn name(&self) -> &'static str {
        "PxeServiceProvider"
    }

    fn register(&self, app: &Application) -> Result<()> {
        let settings = app.resolve::<Config>()?;

        // Empty until `boot` below reads it from the database, which cannot
        // happen before the migrations that create its tables have run.
        let policy = Arc::new(PolicyRepository::new(self.database.clone()));
        let store = RuleStore::database(Arc::clone(&policy)).with_live(self.live.clone());

        let ouis = load_ouis(&settings);

        let server_settings = ServerSettings {
            server_ip: settings
                .get_or(PXE_SERVER_IP, "127.0.0.1".to_string())
                .parse()
                .map_err(|_| Error::internal("pxe.server_ip is not an IPv4 address"))?,
            http_base: settings.get_or(PXE_HTTP_BASE, "http://127.0.0.1:8080".to_string()),
            description: settings.get_or(PXE_DESCRIPTION, "kindling netboot".to_string()),
        };

        let store = Arc::new(store);
        let ouis = Arc::new(ouis);
        // The inventory and the log announce their own writes, so every path
        // into them — three protocols, the API, a rule's tags — is live
        // without any of those paths having to remember to be.
        let hosts =
            Arc::new(HostRepository::new(self.database.clone()).with_live(self.live.clone()));
        let events =
            Arc::new(BootEventRepository::new(self.database.clone()).with_live(self.live.clone()));

        // The same directory TFTP serves, so a loader dropped in once is
        // reachable over both protocols. `PublicFiles` refuses anything that
        // resolves outside it, which is the same guarantee the TFTP side makes
        // — stated twice because they are two different pieces of code.
        let tftp_root = settings.get_or(PXE_TFTP_ROOT, "tftproot".to_string());

        app.instance_arc(Arc::clone(&store));
        app.instance_arc(policy);
        app.instance_arc(Arc::clone(&ouis));
        app.instance_arc(Arc::clone(&hosts));
        app.instance_arc(Arc::clone(&events));
        app.instance(self.live.clone());
        app.instance(PublicFiles::at(tftp_root).cached_for("public, max-age=300"));

        // A threshold below two turns the breaker off, for a deployment that
        // would rather have a loop than a server that ever second-guesses its
        // own answer.
        let loops = Arc::new(LoopBreaker::new(
            std::time::Duration::from_secs(settings.get_or(PXE_LOOP_WINDOW_SECS, 90u64).max(1)),
            settings.get_or(PXE_LOOP_THRESHOLD, 4usize),
            4096,
        ));

        app.instance_arc(Arc::clone(&loops));
        app.instance_arc(Arc::new(BootService::new(
            store,
            ouis,
            server_settings,
            hosts,
            events,
            loops,
        )));

        app.instance(crate::database::migrations::all());
        Ok(())
    }

    rainier_framework::container::boot_provider!(async |self, app| {
        // Migrations run at boot rather than in a deploy step, because this
        // server is routinely installed by one person on one machine and a
        // forgotten `migrate` would look like an inventory that never records
        // anything.
        if running_a_migration_command() {
            return Ok(());
        }

        let database = app.resolve::<Database>()?;
        let migrator = app.resolve::<rainier_framework::database::Migrator>()?;
        let applied = migrator.run(&database).await?;
        if !applied.is_empty() {
            tracing::info!(count = applied.len(), "applied migrations");
        }

        // The policy. On an empty database a policy file from an earlier
        // release is imported once — so an upgrade keeps its policy — and
        // otherwise the starter is seeded.
        let settings = app.resolve::<Config>()?;
        let import = crate::app::services::rule_store::legacy_policy_path(
            &settings.get_or(PXE_RULES_PATH, String::new()),
        );
        let store = app.resolve::<RuleStore>()?;
        store.load(import.as_deref()).await.map_err(|e| {
            Error::internal(format!(
                "the boot policy will not load, so this server would tell machines to boot \
                 nothing:\n{e}"
            ))
        })?;

        // Said once, at boot, where it can still be acted on. The endpoints
        // themselves fail closed, so this is a warning and not an error.
        if settings.get_or(PXE_API_TOKEN, String::new()).trim().is_empty() {
            tracing::warn!(
                "PXE_API_TOKEN is not set, so the endpoints that pin machines and reload the \
                 policy are closed. Reading the inventory still works."
            );
        }

        Ok(())
    });
}

/// `migrate` running the migrations twice would be harmless but confusing, and
/// `migrate:rollback` running them first would undo its own work.
fn running_a_migration_command() -> bool {
    std::env::args().nth(1).is_some_and(|command| command.starts_with("migrate"))
}

/// The built-in vendor table, plus a file if one was named.
///
/// A missing or unreadable file is a warning rather than a failure: vendor
/// names decorate the inventory and feed `vendor =` rules, and a server that
/// refused to start over a missing decoration would be refusing to boot a
/// fleet over it.
fn load_ouis(settings: &Config) -> OuiDatabase {
    let path = settings.get_or(PXE_OUI_FILE, String::new());
    if path.trim().is_empty() {
        return OuiDatabase::new();
    }

    match OuiDatabase::load(&path) {
        Ok(database) => {
            tracing::info!(
                path,
                loaded = database.loaded(),
                built_in = OuiDatabase::built_in(),
                "vendor list loaded"
            );
            database
        }
        Err(e) => {
            tracing::warn!(
                path,
                error = %e,
                "the vendor list could not be read; using the built-in table only. Rules \
                 matching on `vendor` may not fire for hardware outside it."
            );
            OuiDatabase::new()
        }
    }
}
