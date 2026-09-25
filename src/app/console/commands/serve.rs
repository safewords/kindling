//! `pxe:serve` — every listener this server has, in one process.
//!
//! Network boot is three protocols and they only work together. Starting them
//! as three processes would mean three ways to be half-started, and the
//! symptom of each is the same: a machine that hangs with no message. So they
//! come up together, and if one of them cannot bind, none of them serve.
//!
//! The exception is deliberate: DHCP and TFTP can each be turned *off* in
//! configuration, because a network that already has a DHCP server with boot
//! options, or a TFTP server, wants this one to stay out of the way. Off is a
//! decision; failing to bind is not.

use std::net::Ipv4Addr;
use std::sync::Arc;

use rainier_framework::console_kernel::{exit, Arguments, Command};
use rainier_framework::prelude::*;
use rainier_framework::server::{Kernel, Server, ServerOptions};

use crate::app::services::BootService;
use crate::config::keys::*;
use crate::pxe::dhcp::proxy::ProxyDhcpServer;
use crate::pxe::tftp::server::{Limits, TftpServer};

#[derive(Debug, Default)]
pub struct ServeCommand;

#[async_trait]
impl Command for ServeCommand {
    fn name(&self) -> &str {
        "pxe:serve"
    }

    fn description(&self) -> &str {
        "Serve network boot: proxy DHCP, TFTP and HTTP together"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  pxe:serve [--host=0.0.0.0] [--port=8080] [--no-dhcp] [--no-tftp]\n\n\
             Ports 67, 69 and 4011 need root (CAP_NET_BIND_SERVICE), or an elevated\n\
             prompt on Windows. The HTTP port does not.\n\n\
             This server hands out no IP addresses. It answers the boot half of a DHCP\n\
             conversation and leaves the addressing to whatever already does it.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let settings = app.resolve::<rainier_framework::config::Config>()?;
        let service = app.resolve::<BootService>()?;

        let host = args
            .option("host")
            .map(str::to_string)
            .unwrap_or_else(|| settings.get_or(SERVER_HOST, "0.0.0.0".to_string()));
        let port: u16 = args.parsed_or("port", settings.get_or(SERVER_PORT, 8080u16));

        let dhcp = !args.flag("no-dhcp") && settings.get_or(PXE_DHCP_ENABLED, true);
        let tftp = !args.flag("no-tftp") && settings.get_or(PXE_TFTP_ENABLED, true);

        let server_ip = service.settings().server_ip;
        let http_base = service.settings().http_base.clone();
        let tftp_root = settings.get_or(PXE_TFTP_ROOT, "tftproot".to_string());

        banner(&service, server_ip, &http_base, &tftp_root, dhcp, tftp);

        // The privileged UDP ports are bound here, before anything serves. A
        // port that was already taken is then a message at a terminal
        // somebody is looking at, rather than a rack that was offered a file
        // and could not fetch it.
        let mut listeners: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>> =
            Vec::new();

        if dhcp {
            let bind: Ipv4Addr = settings
                .get_or(PXE_DHCP_BIND, "0.0.0.0".to_string())
                .parse()
                .map_err(|_| Error::internal("PXE_DHCP_BIND is not an IPv4 address"))?;

            let proxy = ProxyDhcpServer::new(
                service.settings().clone(),
                Arc::clone(&service) as Arc<dyn crate::pxe::dhcp::proxy::BootPolicy>,
                Arc::clone(service.ouis()),
            );
            let boot_server_port = settings.get_or(PXE_DHCP_BOOT_SERVER_PORT, true);

            // Bound here, in the command, rather than inside the future: a
            // future is not polled until everything is assembled, and a port
            // this server cannot have should be a message now rather than a
            // silence later.
            let bound = proxy
                .bind(bind, boot_server_port)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;

            listeners.push(Box::pin(async move {
                bound.serve().await.map_err(|e| Error::internal(e.to_string()))
            }));
        }

        if tftp {
            let bind: Ipv4Addr = settings
                .get_or(PXE_TFTP_BIND, "0.0.0.0".to_string())
                .parse()
                .map_err(|_| Error::internal("PXE_TFTP_BIND is not an IPv4 address"))?;
            let tftp_port = settings.get_or(PXE_TFTP_PORT, 69u16);

            let files = TftpServer::new(
                &tftp_root,
                Arc::clone(&service) as Arc<dyn crate::pxe::tftp::server::TftpEvents>,
            )
            .with_limits(Limits {
                max_block_size: settings.get_or(PXE_TFTP_MAX_BLOCK_SIZE, 1468u16),
                max_window_size: settings.get_or(PXE_TFTP_MAX_WINDOW_SIZE, 16u16),
                ..Limits::default()
            });

            let bound = files
                .bind((bind, tftp_port).into())
                .await
                .map_err(|e| Error::internal(e.to_string()))?;

            listeners.push(Box::pin(async move {
                bound.serve().await.map_err(|e| Error::internal(e.to_string()))
            }));
        }

        // The HTTP kernel, the same one `serve` runs.
        let kernel = app.resolve::<Kernel>()?;
        let options = ServerOptions::default()
            .bind_to(&host, port)?
            .max_body_bytes(settings.get_or(SERVER_MAX_BODY_BYTES, 256 * 1024u64) as usize)
            .trust_forwarded_for(settings.get_or("server.trust_proxy", false));

        let mut http = Server::from_arc(kernel).with_options(options);

        // Each connection is served in a spawned task, and a spawned task
        // inherits no facade scope — so without this a handler would resolve
        // through whatever is installed process-wide rather than through this
        // application.
        if let Some(application) = rainier_framework::container::try_facade_application() {
            http = http.for_application(application);
        }

        // The admin interface's live feed. Sockets share the HTTP listener —
        // an upgrade is a request the same accept loop takes — so this is the
        // whole of serving them, and they need no port of their own.
        if let Ok(sockets) = app.resolve::<rainier_framework::websocket::WebSocketRoutes>() {
            if !sockets.is_empty() {
                http = http.with_websockets(sockets);
            }
        }

        listeners.push(Box::pin(async move {
            http.run().await.map_err(|e| Error::internal(e.to_string()))
        }));

        // The first listener to stop, stops the server. One of them failing is
        // not a state worth continuing in: a boot server serving two of its
        // three protocols is a boot server that hangs machines.
        let outcome = first_to_stop(listeners).await;

        app.terminate();

        match outcome {
            Ok(()) => Ok(exit::SUCCESS),
            Err(e) => {
                eprintln!("\n{}", e.message());
                Ok(exit::FAILURE)
            }
        }
    }
}

/// Wait for the first listener to stop, whichever it is.
///
/// `select_all` without pulling in `futures` for it: three futures, none of
/// which completes in the normal case, polled until one does. Dropping the
/// rest is the intent — a boot server serving two of its three protocols
/// hangs machines more confusingly than one that is plainly down.
async fn first_to_stop(
    listeners: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>>,
) -> Result<()> {
    use std::future::poll_fn;
    use std::task::Poll;

    let mut listeners = listeners;
    poll_fn(|context| {
        for listener in listeners.iter_mut() {
            if let Poll::Ready(outcome) = listener.as_mut().poll(context) {
                return Poll::Ready(outcome);
            }
        }
        Poll::Pending
    })
    .await
}

fn banner(
    service: &BootService,
    server_ip: Ipv4Addr,
    http_base: &str,
    tftp_root: &str,
    dhcp: bool,
    tftp: bool,
) {
    let rules = service.rules().current();

    println!("kindling");
    println!("  machines reach this server at  {server_ip}");
    println!("  http                           {http_base}");
    println!("  boot files                     {tftp_root}");
    println!(
        "  policy                         {} ({} rule(s), {} profile(s))",
        rules.source(),
        rules.rules().len(),
        rules.profiles().len()
    );
    println!("  proxy DHCP                     {}", if dhcp { "on (67, 4011)" } else { "off" });
    println!("  TFTP                           {}", if tftp { "on (69)" } else { "off" });

    if rules.rules().is_empty() && rules.settings().default_profile.is_none() {
        println!(
            "\n  ! No rules and no default profile, so no machine will be told to boot\n    \
             anything. Add rules on the Policy screen of the web UI."
        );
    }

    println!("\nCtrl-C to stop.");
}
