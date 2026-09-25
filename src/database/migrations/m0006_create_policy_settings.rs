use rainier_framework::database::Step;

use crate::app::models::PolicySetting;

pub fn migration() -> Step {
    Step::create_table::<PolicySetting>("0006_create_policy_settings")
}
