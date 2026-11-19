//! The console entry point.
//!
//! ```sh
//! cargo run -- list                    # every command
//! cargo run -- pxe:serve               # DHCP, TFTP and HTTP together
//! cargo run -- pxe:rules               # the boot policy in force
//! cargo run -- pxe:rule add …          # change it (also pxe:profile, pxe:policy)
//! cargo run -- pxe:test --mac=…        # what would this machine boot, and why
//! cargo run -- pxe:hosts               # the inventory
//! ```

use std::path::Path;

use pxe::config::service_file;
use pxe::{boot, Mode};

fn main() {
    // Everything here happens before the runtime exists, on the only thread
    // there is — which is what makes writing to the process environment safe.
    if let Err(message) = prepare_environment() {
        eprintln!("kindling could not start:\n{message}");
        std::process::exit(1);
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("kindling could not start a runtime: {e}");
            std::process::exit(1);
        }
    };
    let code = runtime.block_on(run());
    std::process::exit(code);
}

async fn run() -> i32 {
    // Not `boot(..).await?`. Returning the error from `main` prints it with
    // `Debug`, and the commonest failure here is a setting with a typo in it —
    // which deserves its own sentence, not a struct dump.
    let application = match boot(Mode::Running).await {
        Ok(application) => application,
        Err(e) => {
            eprintln!("kindling could not start:\n{}", e.message());
            return 1;
        }
    };

    let code = pxe::routes::console::commands()
        .run_from_env(&application)
        .await;

    application.terminate();
    code
}

/// Read `kindling.toml` into the process environment, under anything that is
/// really set there, so every part of the server — the framework's own
/// settings included — reads the same values.
fn prepare_environment() -> std::result::Result<(), String> {
    let path = service_file::path();
    convert_dotenv(&path)?;

    let env = service_file::load(&path).map_err(|e| e.to_string())?;
    for (variable, value) in env.file_vars() {
        if std::env::var_os(variable).is_none() {
            std::env::set_var(variable, value);
        }
    }

    quieten_sql_logging();
    Ok(())
}

/// An earlier release read `.env`. On the first start without a
/// `kindling.toml`, its settings are converted into one and `.env` is moved
/// aside — once, and said on the terminal. With both present the server
/// refuses, because two files saying different things is the problem this
/// replaced.
fn convert_dotenv(path: &Path) -> std::result::Result<(), String> {
    let dotenv = Path::new(".env");
    if !dotenv.exists() {
        return Ok(());
    }
    if path.exists() {
        return Err(format!(
            "both `.env` and `{}` are here. kindling reads only `{}` now; move `.env` out of \
             the way (its settings belong in `{}`, or in the real environment).",
            path.display(),
            path.display(),
            path.display()
        ));
    }

    let text =
        std::fs::read_to_string(dotenv).map_err(|e| format!("could not read `.env`: {e}"))?;
    std::fs::write(path, service_file::from_dotenv(&text))
        .map_err(|e| format!("could not write `{}`: {e}", path.display()))?;
    std::fs::rename(dotenv, ".env.converted")
        .map_err(|e| format!("could not move `.env` aside: {e}"))?;
    eprintln!(
        "kindling: converted `.env` into `{}` and moved it to `.env.converted`. Service settings \
         are read from `{}` from now on. A policy file it named (PXE_RULES_PATH) is not read: the \
         policy lives in the database.",
        path.display(),
        path.display()
    );
    Ok(())
}

/// `sqlx` logs every statement at INFO, and this server runs a handful per
/// machine per boot. Left alone, a rack coming up buries the one line that
/// says which image each machine was sent under a thousand `SELECT`s.
///
/// Only when nobody has said otherwise: `RUST_LOG` in the environment, or
/// `[log] filter` in `kindling.toml`, is a deliberate choice and wins.
fn quieten_sql_logging() {
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "info,sqlx=warn");
    }
}
