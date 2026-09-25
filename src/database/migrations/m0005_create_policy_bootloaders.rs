use rainier_framework::database::Step;

use crate::app::models::PolicyBootloader;

pub fn migration() -> Step {
    Step::create_table::<PolicyBootloader>("0005_create_policy_bootloaders")
}
