//! Migrations, in the order they run. Nothing is discovered from a filename
//! scan, so the order is a list you can read.

use rainier_framework::database::Migrator;

pub mod m0001_create_hosts;
pub mod m0002_create_boot_events;
pub mod m0003_create_policy_rules;
pub mod m0004_create_policy_profiles;
pub mod m0005_create_policy_bootloaders;
pub mod m0006_create_policy_settings;
pub mod m0007_create_policy_revisions;
pub mod m0008_create_rule_templates;

pub fn all() -> Migrator {
    Migrator::new()
        .add(m0001_create_hosts::migration())
        .add(m0002_create_boot_events::migration())
        // The boot policy, which used to be a TOML file beside the binary.
        .add(m0003_create_policy_rules::migration())
        .add(m0004_create_policy_profiles::migration())
        .add(m0005_create_policy_bootloaders::migration())
        .add(m0006_create_policy_settings::migration())
        .add(m0007_create_policy_revisions::migration())
        .add(m0008_create_rule_templates::migration())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rainier_framework::database::Dialect;

    #[test]
    fn migrations_are_named_in_order() {
        assert_eq!(
            all().names(),
            vec![
                "0001_create_hosts",
                "0002_create_boot_events",
                "0003_create_policy_rules",
                "0004_create_policy_profiles",
                "0005_create_policy_bootloaders",
                "0006_create_policy_settings",
                "0007_create_policy_revisions",
                "0008_create_rule_templates",
            ]
        );
    }

    #[test]
    fn the_module_prefix_matches_the_migration_name() {
        for (module, name) in ["m0001_create_hosts", "m0002_create_boot_events", "m0003_create_policy_rules", "m0004_create_policy_profiles", "m0005_create_policy_bootloaders", "m0006_create_policy_settings", "m0007_create_policy_revisions", "m0008_create_rule_templates"]
            .iter()
            .zip(all().names())
        {
            assert_eq!(module.trim_start_matches('m'), name, "`{module}.rs` declares `{name}`");
        }
    }

    #[test]
    fn every_step_can_be_rolled_back() {
        assert!(
            all().irreversible(Dialect::Sqlite).is_empty(),
            "a boot server's schema should be reversible: it is a handful of tables"
        );
    }
}
