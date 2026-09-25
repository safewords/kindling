use rainier_framework::database::Step;

use crate::app::models::PolicyRevision;

pub fn migration() -> Step {
    Step::create_table::<PolicyRevision>("0007_create_policy_revisions")
}
