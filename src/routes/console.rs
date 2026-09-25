//! The console, and the one scheduled task this server has.

use rainier_framework::console_kernel::Console;
use rainier_framework::scheduler::Schedule;

use crate::app::console::commands::{
    DoctorCommand, HostsCommand, IpxeCommand, LogCommand, PinCommand, RulesCommand, ServeCommand,
    TagCommand, TestCommand,
};

pub fn commands() -> Console {
    rainier_framework::console("pxe")
        .register(ServeCommand)
        .register(RulesCommand)
        .register(TestCommand)
        .register(DoctorCommand)
        .register(HostsCommand)
        .register(PinCommand)
        .register(TagCommand)
        .register(LogCommand)
        .register(IpxeCommand)
}

pub fn schedule(schedule: &mut Schedule) {
    // The boot log is the only thing here that grows without bound: a busy
    // network writes a handful of rows per machine per boot, and nothing ever
    // reads a row from six months ago.
    schedule
        .call("pxe:prune-events", |app| {
            Box::pin(async move {
                use crate::app::repositories::BootEventRepository;
                use crate::config::keys::PXE_EVENT_RETENTION_DAYS;

                let settings = app.resolve::<rainier_framework::config::Config>()?;
                let days = settings.get_or(PXE_EVENT_RETENTION_DAYS, 30i64);

                let events = app.resolve::<BootEventRepository>()?;
                let pruned = events.prune(days).await?;
                if pruned > 0 {
                    tracing::info!(pruned, days, "old boot events removed");
                }
                Ok(())
            })
        })
        .daily()
        .described_as("Drop boot events past the retention window");
    // No `without_overlapping`, deliberately. That guard needs a shared cache,
    // and a boot server is normally one process on one machine — requiring
    // Redis so a nightly `DELETE … WHERE at < cutoff` cannot run twice would
    // be adding a dependency to protect an idempotent statement from itself.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_ins_and_this_applications_own_are_all_registered() {
        let console = commands();
        for name in [
            "serve",
            "route:list",
            "migrate",
            "pxe:serve",
            "pxe:rules",
            "pxe:test",
            "pxe:doctor",
            "pxe:hosts",
            "pxe:pin",
            "pxe:tag",
            "pxe:log",
            "pxe:ipxe",
        ] {
            assert!(console.find(name).is_some(), "`{name}` should be registered");
        }
    }

    #[test]
    fn every_scheduled_expression_parses_and_no_two_tasks_share_a_name() {
        let mut built = Schedule::new();
        schedule(&mut built);

        assert!(built.errors().is_empty(), "{:?}", built.errors());
        assert!(built.duplicate_names().is_empty());
    }

    #[test]
    fn nothing_scheduled_needs_a_lock_this_deployment_cannot_have() {
        // A guard here would need a shared cache, and this server is normally
        // one process on one machine. The pruning statement is idempotent, so
        // running it twice is not a thing to add Redis to prevent.
        let mut built = Schedule::new();
        schedule(&mut built);

        for task in built.tasks() {
            assert!(
                task.overlap_ttl().is_none() && !task.is_one_server(),
                "`{}` asks for a distributed lock; see the note in `schedule`",
                task.name()
            );
        }
    }
}
