//! The commands this application adds to the console.

pub mod doctor;
pub mod inventory;
pub mod ipxe;
pub mod rules;
pub mod serve;
pub mod test;

pub use doctor::DoctorCommand;
pub use inventory::{HostsCommand, LogCommand, PinCommand, TagCommand};
pub use ipxe::IpxeCommand;
pub use rules::RulesCommand;
pub use serve::ServeCommand;
pub use test::TestCommand;
