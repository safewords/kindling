use rainier_framework::database::Step;

use crate::app::models::Host;

pub fn migration() -> Step {
    Step::create_table::<Host>("0001_create_hosts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rainier_framework::database::{Dialect, Down, Migration};

    #[test]
    fn the_address_is_unique_because_it_is_the_identity_of_the_row() {
        let up = migration().up(Dialect::Sqlite).join("\n");
        assert!(up.contains("hosts"), "{up}");
        assert!(up.to_lowercase().contains("unique"), "the MAC must be unique: {up}");
    }

    #[test]
    fn it_undoes_itself() {
        assert_eq!(
            migration().down(Dialect::Sqlite),
            Down::Statements(vec!["DROP TABLE IF EXISTS hosts".to_string()])
        );
    }
}
