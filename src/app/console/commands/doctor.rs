//! `pxe:doctor` — everything that silently does not work, found on purpose.
//!
//! Network boot fails quietly. A machine offered a boot file that is not there
//! gets a TFTP error it does not display and falls through to its next boot
//! device; a machine offered a binary for the wrong architecture loads it,
//! fails to execute it and does exactly the same. Both look, from the server,
//! like a machine that simply never came back.
//!
//! So this command asks the questions a machine would ask, and answers them
//! against the disk: for every architecture this server says it serves, is the
//! file actually there? Does every kernel a profile names exist? Can the
//! privileged ports be taken?

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};

use rainier_framework::config::Config;
use rainier_framework::console_kernel::{exit, Arguments, Command};
use rainier_framework::prelude::*;

use super::ipxe;
use crate::app::services::RuleStore;
use crate::config::keys::*;
use crate::pxe::policy::{default_bootloader, BOOT_PATH};
use crate::pxe::profile::ProfileKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Ok,
    Warn,
    Fail,
}

impl Verdict {
    fn marker(&self) -> &'static str {
        match self {
            Verdict::Ok => "ok  ",
            Verdict::Warn => "warn",
            Verdict::Fail => "FAIL",
        }
    }
}

struct Report {
    findings: Vec<(Verdict, String, String)>,
}

impl Report {
    fn new() -> Self {
        Self { findings: Vec::new() }
    }

    fn note(&mut self, verdict: Verdict, check: impl Into<String>, detail: impl Into<String>) {
        self.findings.push((verdict, check.into(), detail.into()));
    }

    fn ok(&mut self, check: impl Into<String>, detail: impl Into<String>) {
        self.note(Verdict::Ok, check, detail);
    }

    fn warn(&mut self, check: impl Into<String>, detail: impl Into<String>) {
        self.note(Verdict::Warn, check, detail);
    }

    fn fail(&mut self, check: impl Into<String>, detail: impl Into<String>) {
        self.note(Verdict::Fail, check, detail);
    }

    fn worst(&self) -> Verdict {
        if self.findings.iter().any(|(verdict, _, _)| *verdict == Verdict::Fail) {
            Verdict::Fail
        } else if self.findings.iter().any(|(verdict, _, _)| *verdict == Verdict::Warn) {
            Verdict::Warn
        } else {
            Verdict::Ok
        }
    }

    fn print(&self) {
        for (verdict, check, detail) in &self.findings {
            println!("  [{}] {check}", verdict.marker());
            if !detail.is_empty() {
                for line in detail.lines() {
                    println!("         {line}");
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct DoctorCommand;

#[async_trait]
impl Command for DoctorCommand {
    fn name(&self) -> &str {
        "pxe:doctor"
    }

    fn description(&self) -> &str {
        "Check everything a machine needs before a machine has to find out"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  pxe:doctor [--ports]\n\n\
             Options:\n  --ports  Also try to bind 67, 69 and 4011\n\n\
             Exits non-zero if anything would stop a machine booting, so it can run\n\
             in a deploy pipeline. Warnings do not fail it.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let settings = app.resolve::<Config>()?;
        let store = app.resolve::<RuleStore>()?;
        let rules = store.current();

        let root = PathBuf::from(settings.get_or(PXE_TFTP_ROOT, "tftproot".to_string()));
        let base = settings.get_or(PXE_HTTP_BASE, String::new());
        let mut report = Report::new();

        // --- the policy ----------------------------------------------------
        match store.last_error() {
            Some((at, message)) => report.fail(
                "the stored policy does not load",
                format!(
                    "what is running is older than what is stored. Last tried {}:\n{message}",
                    at.format("%Y-%m-%d %H:%M:%S UTC")
                ),
            ),
            None => report.ok(
                format!("policy: {} rule(s), {} profile(s)", rules.rules().len(), rules.profiles().len()),
                format!(
                    "from the {}{}",
                    rules.source(),
                    store.revision().map(|r| format!(", revision {r}")).unwrap_or_default()
                ),
            ),
        }

        if rules.rules().is_empty() && rules.settings().default_profile.is_none() {
            report.fail(
                "nothing would be told to boot anything",
                "there are no rules and no default profile. Add them on the Policy screen, \
                 or `pxe:rules --import=FILE`.",
            );
        }

        // --- the boot root -------------------------------------------------
        if root.is_dir() {
            report.ok(format!("boot root: {}", root.display()), "");
        } else {
            report.fail(
                format!("boot root `{}` is not a directory", root.display()),
                "TFTP and /boot both serve this, so nothing can be fetched until it exists.",
            );
        }

        // --- a loader per architecture --------------------------------------
        //
        // The check this command exists for. Option 93 decides which binary a
        // machine can execute, and the wrong one — or a missing one — is a
        // machine that falls through to its disk without a word.
        let mut checked: BTreeSet<String> = BTreeSet::new();

        // Grouped by file rather than by architecture: `x64-uefi` and
        // `default` are routinely the same binary, and saying so twice makes a
        // reader check whether they are really two problems.
        let mut by_file: std::collections::BTreeMap<&String, Vec<&String>> =
            std::collections::BTreeMap::new();
        for (arch, file) in rules.bootloaders() {
            checked.insert(arch.clone());
            by_file.entry(file).or_default().push(arch);
        }

        for (file, arches) in by_file {
            let path = root.join(file);
            let listed = arches.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", ");

            if path.is_file() {
                report.ok(format!("loader for {listed}: {file}"), "");
            } else {
                let whose = match arches.as_slice() {
                    [only] if *only == "default" => {
                        "Any machine whose architecture is not named above".to_string()
                    }
                    _ => format!("A machine of {listed}"),
                };
                report.fail(
                    format!("loader for {listed} is missing: {file}"),
                    format!(
                        "`{}` does not exist. {whose} would be offered it, fail to fetch it, and \
                         fall through to its next boot device silently.\n\
                         Download it from https://boot.ipxe.org/",
                        path.display()
                    ),
                );
            }
        }

        // The two architectures nearly every fleet has. Not declared means the
        // built-in default applies, and it is still a file that has to exist —
        // but a warning rather than a failure, since a site may genuinely have
        // no machines of that kind.
        for arch in ["bios", "x64-uefi"] {
            if checked.contains(arch) || rules.bootloaders().contains_key("default") {
                continue;
            }
            let file = default_bootloader(arch);
            if !root.join(file).is_file() {
                report.warn(
                    format!("no loader for `{arch}`"),
                    format!(
                        "`[bootloaders]` does not name one, and the default `{file}` is not in \
                         the boot root. Any {arch} machine that boots here gets nothing."
                    ),
                );
            }
        }

        for (name, profile) in rules.profiles() {
            if let Some(file) = profile.bootfile_for("default").or_else(|| profile.bootfile_for("bios")) {
                if !file.starts_with("http") && !root.join(file).is_file() {
                    report.fail(
                        format!("profile `{name}` names a boot file that is not there: {file}"),
                        format!("expected `{}`", root.join(file).display()),
                    );
                }
            }
        }

        // --- what the profiles point at -------------------------------------
        for (name, profile) in rules.profiles() {
            if profile.kind() != ProfileKind::Kernel {
                continue;
            }

            let referenced = profile
                .kernel
                .iter()
                .chain(profile.initrd.iter())
                .filter_map(|target| local_path(target, &root, &base));

            for (target, path) in referenced {
                if path.is_file() {
                    report.ok(format!("profile `{name}`: {target}"), "");
                } else {
                    report.warn(
                        format!("profile `{name}` points at a file that is not there"),
                        format!("`{target}` → `{}`", path.display()),
                    );
                }
            }
        }

        // --- where machines think this server is ----------------------------
        let server_ip = settings.get_or(PXE_SERVER_IP, String::new());
        match server_ip.parse::<IpAddr>() {
            Ok(address) if address.is_loopback() => report.warn(
                format!("machines are told to come back to {address}"),
                "which is this machine talking to itself. Set PXE_SERVER_IP to an address the \
                 machines can reach.",
            ),
            Ok(address) => report.ok(format!("machines are told to come back to {address}"), base.clone()),
            Err(_) => report.fail(
                format!("PXE_SERVER_IP is `{server_ip}`, which is not an address"),
                "",
            ),
        }

        // --- this project's own iPXE ----------------------------------------
        //
        // A custom build compiles in the server it comes back to. Built for
        // another address — an old one, a test box — it sends machines to
        // somebody else's policy, or to nothing, while everything here still
        // looks healthy.
        let custom = ipxe::builds(&root);
        if custom.is_empty() {
            report.ok(
                "no custom iPXE build; the loaders in the boot root are stock or hand-placed",
                "`pxe:ipxe --build` compiles one that cannot chainload itself in a loop.",
            );
        }
        for build in &custom {
            let Some(info) = &build.info else {
                report.warn(
                    format!("the {} iPXE build has no BUILD-INFO", build.arch),
                    format!(
                        "`{}` looks like an interrupted build. `pxe:ipxe --build --arch={}` redoes it.",
                        build.dir.display(),
                        build.arch
                    ),
                );
                continue;
            };

            let in_service: Vec<&str> = ipxe::INSTALLS
                .iter()
                .filter(|(arch, from, name)| {
                    *arch == build.arch
                        && ipxe::installed_state(&build.dir.join(from), &root.join(name)) == ipxe::Installed::Same
                })
                .map(|(_, _, name)| *name)
                .collect();
            let placement = if in_service.is_empty() {
                "built, not installed (`pxe:ipxe --install`)".to_string()
            } else {
                format!("installed as {}", in_service.join(", "))
            };

            match ipxe::check_base(info.embed_base(), &base) {
                ipxe::BaseCheck::Matches => {
                    report.ok(format!("the {} iPXE build comes back to {base}", build.arch), placement)
                }
                ipxe::BaseCheck::Discovers => report.ok(
                    format!("the {} iPXE build finds the server through DHCP", build.arch),
                    placement,
                ),
                ipxe::BaseCheck::Differs => report.warn(
                    format!(
                        "the {} iPXE build comes back to {}, not to this server",
                        build.arch,
                        info.embed_base().unwrap_or_default()
                    ),
                    format!(
                        "PXE_HTTP_BASE is {base}. {} — machines running it ask that server, not this \
                         one. `pxe:ipxe --build --arch={}` rebuilds it for this one.",
                        placement, build.arch
                    ),
                ),
            }
        }

        if settings.get_or(PXE_API_TOKEN, String::new()).trim().is_empty() {
            report.warn(
                "PXE_API_TOKEN is not set",
                "The endpoints that pin machines and reload the policy are closed. Reading is \
                 unaffected. Set one with `openssl rand -hex 32`.",
            );
        } else {
            report.ok("the management API is guarded by a token", "");
        }

        // --- the ports ------------------------------------------------------
        if args.flag("ports") {
            let bind = settings.get_or(PXE_DHCP_BIND, "0.0.0.0".to_string());
            let dhcp = settings.get_or(PXE_DHCP_ENABLED, true);
            let tftp = settings.get_or(PXE_TFTP_ENABLED, true);

            if dhcp {
                check_port(&mut report, &bind, 67, "proxy DHCP");
                if settings.get_or(PXE_DHCP_BOOT_SERVER_PORT, true) {
                    check_port(&mut report, &bind, 4011, "the PXE boot server port");
                }
            }
            if tftp {
                check_port(&mut report, &bind, settings.get_or(PXE_TFTP_PORT, 69u16), "TFTP");
            }
        }

        println!();
        report.print();
        println!();

        match report.worst() {
            Verdict::Fail => {
                println!("Something here would stop a machine booting.");
                Ok(exit::FAILURE)
            }
            Verdict::Warn => {
                println!("Nothing fatal, but read the warnings above.");
                Ok(exit::SUCCESS)
            }
            Verdict::Ok => {
                println!("Ready.");
                Ok(exit::SUCCESS)
            }
        }
    }
}

/// Turn a URL a profile points at into the local file it would be served from,
/// when it is one this server serves at all.
///
/// A profile is free to point anywhere — a mirror, an S3 bucket — and those
/// cannot be checked from here. What can be checked is the common case: a file
/// under this server's own boot root.
fn local_path(target: &str, root: &Path, base: &str) -> Option<(String, PathBuf)> {
    let over_http = format!("{}/{BOOT_PATH}/", base.trim_end_matches('/'));

    let relative = target
        .strip_prefix("{{boot}}/")
        .or_else(|| target.strip_prefix("{{ boot }}/"))
        .or_else(|| target.strip_prefix(&over_http))?;

    // A placeholder that expands per machine cannot be resolved here, and
    // guessing at one would produce a warning about a file that is correct.
    if relative.contains("{{") {
        return None;
    }

    Some((target.to_string(), root.join(relative)))
}

fn check_port(report: &mut Report, bind: &str, port: u16, what: &str) {
    let Ok(address) = bind.parse::<IpAddr>() else {
        report.fail(format!("`{bind}` is not an address to bind"), "");
        return;
    };

    match UdpSocket::bind(SocketAddr::new(address, port)) {
        Ok(_) => report.ok(format!("port {port} is free, for {what}"), ""),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => report.fail(
            format!("port {port} needs privileges, for {what}"),
            "Run as root, grant CAP_NET_BIND_SERVICE, or use an elevated prompt on Windows.",
        ),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => report.fail(
            format!("port {port} is already taken, for {what}"),
            "Another boot server — possibly another copy of this one — is already on it.",
        ),
        Err(e) => report.fail(format!("port {port} cannot be bound, for {what}"), e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_pointing_into_the_boot_root_resolves_to_a_file_on_disk() {
        let root = Path::new("/srv/tftp");
        let base = "http://10.0.0.2:8080";

        let (target, path) = local_path("{{boot}}/images/vmlinuz", root, base).unwrap();
        assert_eq!(target, "{{boot}}/images/vmlinuz");
        assert_eq!(path, root.join("images").join("vmlinuz"));

        // Written out in full, which is the other way people write it.
        let (_, path) =
            local_path("http://10.0.0.2:8080/boot/images/vmlinuz", root, base).unwrap();
        assert_eq!(path, root.join("images").join("vmlinuz"));

        // And with the spaces the placeholder syntax allows.
        assert!(local_path("{{ boot }}/images/vmlinuz", root, base).is_some());
    }

    #[test]
    fn a_profile_pointing_somewhere_else_is_not_checked() {
        // A mirror or a bucket is somebody else's file, and warning about one
        // this server cannot see would be noise on every run.
        let root = Path::new("/srv/tftp");
        assert_eq!(local_path("https://mirror.example/vmlinuz", root, "http://10.0.0.2:8080"), None);
        assert_eq!(local_path("/boot/vmlinuz", root, "http://10.0.0.2:8080"), None);
    }

    #[test]
    fn a_path_that_still_holds_a_placeholder_is_left_alone() {
        // It expands differently per machine, so there is no one file to check
        // and a warning would be about a path that is correct.
        let root = Path::new("/srv/tftp");
        assert_eq!(local_path("{{boot}}/images/{{arch}}/vmlinuz", root, "http://x"), None);
    }

    #[test]
    fn the_worst_finding_decides_the_exit_code() {
        let mut report = Report::new();
        report.ok("fine", "");
        assert_eq!(report.worst(), Verdict::Ok);

        report.warn("hmm", "");
        assert_eq!(report.worst(), Verdict::Warn);

        report.fail("no", "");
        assert_eq!(report.worst(), Verdict::Fail, "a failure outranks any number of warnings");
    }
}
