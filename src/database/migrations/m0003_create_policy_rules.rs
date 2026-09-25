use rainier_framework::database::Step;

use crate::app::models::PolicyRule;

pub fn migration() -> Step {
    Step::create_table::<PolicyRule>("0003_create_policy_rules")
}
