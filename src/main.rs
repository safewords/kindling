//! The console entry point.
//!
//! ```sh
//! cargo run -- list                    # every command
//! cargo run -- pxe:serve               # DHCP, TFTP and HTTP together
//! cargo run -- pxe:rules --example     # a rule file to start from
//! cargo run -- pxe:test --mac=…        # what would this machine boot, and why
//! cargo run -- pxe:hosts               # the inventory
//! ```

use pxe::{boot, Mode};
use rainier_framework::config::Env;
use rainier_framework::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    quieten_sql_logging();

    // Not `boot(..).await?`. Returning the error from `main` prints it with
    // `Debug`, and the commonest failure here is a rule file with a typo in
    // it — which deserves its own sentence, not a struct dump.
    let application = match boot(Mode::Running).await {
        Ok(application) => application,
        Err(e) => {
            eprintln!("kindling could not start:\n{}", e.message());
            std::process::exit(1);
        }
    };

    let code = pxe::routes::console::commands().run_from_env(&application).await;

    application.terminate();
    std::process::exit(code);
}

/// `sqlx` logs every statement at INFO, and this server runs a handful per
/// machine per boot. Left alone, a rack coming up buries the one line that
/// says which image each machine was sent under a thousand `SELECT`s.
///
/// Only when nobody has said otherwise: a `RUST_LOG` in the environment or in
/// `.env` is a deliberate choice and wins.
fn quieten_sql_logging() {
    if std::env::var_os("RUST_LOG").is_some() {
        return;
    }
    if Env::load_or_default(".env").file_vars().contains_key("RUST_LOG") {
        return;
    }

    // Before the runtime has spawned anything, which is what makes writing to
    // the process environment safe here.
    std::env::set_var("RUST_LOG", "info,sqlx=warn");
}
