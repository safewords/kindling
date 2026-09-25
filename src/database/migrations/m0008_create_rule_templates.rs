use rainier_framework::database::Step;

use crate::app::models::RuleTemplate;

pub fn migration() -> Step {
    Step::create_table::<RuleTemplate>("0008_create_rule_templates")
}
