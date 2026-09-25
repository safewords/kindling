use rainier_framework::database::Step;

use crate::app::models::PolicyProfile;

pub fn migration() -> Step {
    Step::create_table::<PolicyProfile>("0004_create_policy_profiles")
}
