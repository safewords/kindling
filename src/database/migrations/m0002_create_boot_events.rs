use rainier_framework::database::Step;

use crate::app::models::BootEvent;

pub fn migration() -> Step {
    Step::create_table::<BootEvent>("0002_create_boot_events")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rainier_framework::database::{Dialect, Migration};

    #[test]
    fn the_log_is_indexed_by_the_two_things_anybody_queries_it_by() {
        // "What happened to this machine" and "what happened just now".
        let up = migration().up(Dialect::Sqlite).join("\n").to_lowercase();
        assert!(up.contains("boot_events"), "{up}");
        assert!(up.contains("index"), "{up}");
    }
}
