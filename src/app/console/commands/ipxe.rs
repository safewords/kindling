//! `pxe:ipxe` — this project's own iPXE: build it, see it, put it in service.
//!
//! Stock iPXE boots whatever the DHCP boot filename says. On a network where
//! that filename is iPXE itself — a router's "Network Boot" option is the usual
//! way — it loads itself for ever, and every exchange along the way looks
//! correct. The build under `ipxe/` embeds a script that knows where this
//! server is and never follows a boot filename back into a loader, so that
//! failure cannot happen at all rather than being detected and broken.
//!
//! Building and installing are separate on purpose. A build writes to
//! `tftproot/ipxe-build/<arch>/`, which nothing is offered; `--install` is the
//! step that changes what machines execute, and it keeps what it replaced.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};

use rainier_framework::config::Config;
use rainier_framework::console_kernel::{exit, io, Arguments, Command};
use rainier_framework::prelude::*;

use crate::config::keys::*;

/// Under the boot root, so a build can be tried on one machine before it is
/// installed for all of them: a profile with
/// `bootfile = { x64-uefi = "ipxe-build/x86_64-efi/ipxe.efi" }`.
pub(crate) const BUILD_DIR: &str = "ipxe-build";

/// Where `--install` keeps what it replaced, one directory per install.
pub(crate) const BACKUP_DIR: &str = "ipxe-backup";

/// Every architecture the build produces, in iPXE's naming.
pub(crate) const ARCHES: [&str; 4] = ["bios", "x86_64-efi", "i386-efi", "arm64-efi"];

/// What `--install` copies where: (architecture, built file, name in the boot
/// root). The names on the right are the ones `[bootloaders]` defaults to and
/// `tftproot/README.md` documents. The i386 and arm64 `snponly.efi` builds
/// have no conventional name there, so they stay under `ipxe-build/`.
pub(crate) const INSTALLS: [(&str, &str, &str); 5] = [
    ("bios", "undionly.kpxe", "undionly.kpxe"),
    ("x86_64-efi", "ipxe.efi", "ipxe.efi"),
    ("x86_64-efi", "snponly.efi", "snponly.efi"),
    ("i386-efi", "ipxe.efi", "ipxe32.efi"),
    ("arm64-efi", "ipxe.efi", "ipxe-arm64.efi"),
];

/// The architecture names this project already uses elsewhere, accepted as
/// well as iPXE's, so `--arch=x64-uefi` means what the policy file means.
pub(crate) fn normalise_arch(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "bios" | "pcbios" | "i386-pcbios" => Some("bios"),
        "x86_64-efi" | "x64-uefi" | "x64" | "x86_64" | "efi64" => Some("x86_64-efi"),
        "i386-efi" | "x86-uefi" | "ia32" | "efi32" => Some("i386-efi"),
        "arm64-efi" | "arm64-uefi" | "arm64" | "aarch64" => Some("arm64-efi"),
        _ => None,
    }
}

/// `--arch=a,b` → the architectures, or the first name that is not one.
fn selected_arches(raw: Option<&str>) -> std::result::Result<Vec<&'static str>, String> {
    let Some(raw) = raw.filter(|raw| !raw.trim().is_empty() && raw.trim() != "all") else {
        return Ok(ARCHES.to_vec());
    };
    let mut chosen = Vec::new();
    for name in raw.split(',').filter(|name| !name.trim().is_empty()) {
        let arch = normalise_arch(name).ok_or_else(|| {
            format!("`{name}` is not an architecture this builds (expected one of: {})", ARCHES.join(", "))
        })?;
        if !chosen.contains(&arch) {
            chosen.push(arch);
        }
    }
    Ok(chosen)
}

/// `BUILD-INFO`, the `key=value` file `ipxe/build.sh` writes beside each build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BuildInfo {
    values: BTreeMap<String, String>,
}

impl BuildInfo {
    pub(crate) fn parse(text: &str) -> Self {
        let values = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
            .collect();
        Self { values }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str).filter(|value| !value.is_empty())
    }

    /// The server URL compiled in, or `None` for a build that finds it
    /// through DHCP.
    pub(crate) fn embed_base(&self) -> Option<&str> {
        self.get("embed_base")
    }

    fn commit_short(&self) -> &str {
        self.get("commit").map(|commit| &commit[..commit.len().min(12)]).unwrap_or("?")
    }
}

/// One architecture's build, as found on disk.
#[derive(Debug, Clone)]
pub(crate) struct Build {
    pub arch: &'static str,
    pub dir: PathBuf,
    /// `None` when the directory exists but its `BUILD-INFO` does not — a
    /// build that was interrupted, or files somebody copied in by hand.
    pub info: Option<BuildInfo>,
}

/// Every architecture that has a build under `<root>/ipxe-build/`.
pub(crate) fn builds(root: &Path) -> Vec<Build> {
    ARCHES
        .iter()
        .map(|arch| (*arch, root.join(BUILD_DIR).join(arch)))
        .filter(|(_, dir)| dir.is_dir())
        .map(|(arch, dir)| {
            let info = std::fs::read_to_string(dir.join("BUILD-INFO")).ok().map(|text| BuildInfo::parse(&text));
            Build { arch, dir, info }
        })
        .collect()
}

/// Whether a build will come back to the server that is configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BaseCheck {
    Matches,
    /// Nothing baked in: it asks DHCP, and so follows the server wherever it
    /// moves. Always fine.
    Discovers,
    /// Baked in, and somewhere else. A machine running it asks that server,
    /// not this one.
    Differs,
}

pub(crate) fn check_base(built: Option<&str>, configured: &str) -> BaseCheck {
    let normalise = |base: &str| base.trim().trim_end_matches('/').to_ascii_lowercase();
    match built {
        None => BaseCheck::Discovers,
        Some(built) if normalise(built) == normalise(configured) => BaseCheck::Matches,
        Some(_) => BaseCheck::Differs,
    }
}

/// Where one installed file stands against its build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Installed {
    /// Not in the boot root at all.
    Absent,
    /// There, and byte-for-byte this build.
    Same,
    /// There, and something else — a stock download, or an older build.
    Other,
}

pub(crate) fn installed_state(built: &Path, installed: &Path) -> Installed {
    match (std::fs::read(built), std::fs::read(installed)) {
        (_, Err(_)) => Installed::Absent,
        (Ok(built), Ok(installed)) if built == installed => Installed::Same,
        _ => Installed::Other,
    }
}

/// One file `--install` would copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    pub arch: &'static str,
    pub from: PathBuf,
    pub to: PathBuf,
    pub state: Installed,
}

/// Everything `--install` would do for these architectures, without doing
/// any of it. An architecture that was asked for and has no build is an
/// error; with no `--arch`, architectures that were never built are skipped.
pub(crate) fn plan_install(
    root: &Path,
    arches: &[&'static str],
    explicit: bool,
) -> std::result::Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for arch in arches {
        let dir = root.join(BUILD_DIR).join(arch);
        let wanted: Vec<_> = INSTALLS.iter().filter(|(a, _, _)| a == arch).collect();
        let missing: Vec<_> = wanted.iter().filter(|(_, file, _)| !dir.join(file).is_file()).collect();

        if !missing.is_empty() {
            if explicit {
                return Err(format!(
                    "there is no {arch} build to install (`{}` is missing). Run `pxe:ipxe --build --arch={arch}` first.",
                    dir.join(missing[0].1).display()
                ));
            }
            continue;
        }

        for (_, file, name) in wanted {
            let from = dir.join(file);
            let to = root.join(name);
            let state = installed_state(&from, &to);
            steps.push(Step { arch, from, to, state });
        }
    }
    Ok(steps)
}

/// Carry out a plan: back up whatever would be overwritten into
/// `<root>/ipxe-backup/<stamp>/`, then replace each file.
///
/// Each file is written beside its destination and renamed over it, so a TFTP
/// transfer that starts mid-install reads the old loader or the new one and
/// never half of each. Returns the backups made.
pub(crate) fn install(root: &Path, steps: &[Step], stamp: &str) -> std::io::Result<Vec<PathBuf>> {
    let backup_dir = root.join(BACKUP_DIR).join(stamp);
    let mut backups = Vec::new();

    for step in steps.iter().filter(|step| step.state != Installed::Same) {
        if step.state == Installed::Other {
            std::fs::create_dir_all(&backup_dir)?;
            let name = step.to.file_name().unwrap_or_default();
            let backup = backup_dir.join(name);
            std::fs::copy(&step.to, &backup)?;
            backups.push(backup);
        }

        let mut staging = step.to.clone().into_os_string();
        staging.push(".installing");
        let staging = PathBuf::from(staging);
        std::fs::copy(&step.from, &staging)?;
        if let Err(e) = std::fs::rename(&staging, &step.to) {
            let _ = std::fs::remove_file(&staging);
            return Err(e);
        }
    }
    Ok(backups)
}

/// A container engine, and whether it can run anything right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineState {
    Missing,
    /// Installed, but its daemon or VM is not up: Docker Desktop not started,
    /// or `podman machine` stopped. The commonest reason a build cannot run,
    /// and one with a one-line fix, so it is told apart from "not installed".
    Stopped,
    Running,
}

fn engine_state(engine: &str) -> EngineState {
    let quietly = |args: &[&str]| {
        Process::new(engine).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()
    };
    match quietly(&["--version"]) {
        Err(e) if e.kind() == ErrorKind::NotFound => EngineState::Missing,
        Err(_) => EngineState::Missing,
        Ok(_) => match quietly(&["info"]) {
            Ok(status) if status.success() => EngineState::Running,
            _ => EngineState::Stopped,
        },
    }
}

/// The engine to build with: the one asked for, or the first running of
/// docker and podman. The error says which are installed but stopped.
fn choose_engine(asked: Option<&str>) -> std::result::Result<String, String> {
    let candidates: Vec<&str> = match asked {
        Some(engine) => vec![engine],
        None => vec!["docker", "podman"],
    };

    let states: Vec<_> = candidates.iter().map(|engine| (*engine, engine_state(engine))).collect();
    if let Some((engine, _)) = states.iter().find(|(_, state)| *state == EngineState::Running) {
        return Ok(engine.to_string());
    }

    let stopped: Vec<_> = states.iter().filter(|(_, s)| *s == EngineState::Stopped).map(|(e, _)| *e).collect();
    if !stopped.is_empty() {
        return Err(format!(
            "{} is installed but not running. Start Docker Desktop, or `podman machine start`, and try \
             again. (On Linux, `ipxe/build.sh --native` builds without a container if the toolchain in \
             `ipxe/Containerfile` is installed.)",
            stopped.join(" and ")
        ));
    }
    Err(match asked {
        Some(engine) => format!("`{engine}` is not installed."),
        None => "neither docker nor podman is installed, and the build needs one: it compiles inside \
                 the container `ipxe/Containerfile` describes, so every host gets the same toolchain."
            .to_string(),
    })
}

/// An absolute path, spelled the way a container engine accepts it in `-v`.
///
/// Not `canonicalize`: on Windows that produces `\\?\C:\…`, which docker
/// reads as a volume name.
fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// An `--embed-base` that iPXE's script parser can use as-is: http(s), and
/// nothing it would split on or try to expand.
fn valid_base(base: &str) -> bool {
    (base.starts_with("http://") || base.starts_with("https://"))
        && base.len() > "https://".len()
        && !base.chars().any(|c| c.is_whitespace() || "$#&|".contains(c))
}

#[derive(Debug, Default)]
pub struct IpxeCommand;

#[async_trait]
impl Command for IpxeCommand {
    fn name(&self) -> &str {
        "pxe:ipxe"
    }

    fn description(&self) -> &str {
        "Build this project's own iPXE, and put it in service"
    }

    fn help(&self) -> Option<&str> {
        Some(
            "Usage:\n  \
             pxe:ipxe                         What is built, and what is installed\n  \
             pxe:ipxe --build [options]       Compile, into tftproot/ipxe-build/\n  \
             pxe:ipxe --install [options]     Copy a build over the loaders in use\n\n\
             Options:\n  \
             --arch=…          bios, x86_64-efi, i386-efi, arm64-efi (comma-separated;\n                    \
             the policy file's names work too). Default: all of them\n  \
             --embed-base=…    The server URL to compile in. Default: PXE_HTTP_BASE.\n                    \
             `none` builds one that finds the server through DHCP\n  \
             --engine=…        docker or podman. Default: whichever is running\n  \
             --dry-run         With --install: say what would change, change nothing\n  \
             --force           With --install: install a build that points at a\n                    \
             different server than this one is configured as\n\n\
             The build runs in a container (ipxe/Containerfile) and needs Docker or\n\
             Podman. See ipxe/README.md.",
        )
    }

    async fn handle(&self, args: &Arguments, app: &Application) -> Result<i32> {
        let settings = app.resolve::<Config>()?;
        let root = PathBuf::from(settings.get_or(PXE_TFTP_ROOT, "tftproot".to_string()));
        let base = settings.get_or(PXE_HTTP_BASE, String::new());

        let arches = match selected_arches(args.option("arch")) {
            Ok(arches) => arches,
            Err(e) => {
                eprintln!("{e}");
                return Ok(exit::FAILURE);
            }
        };

        if args.flag("build") {
            let embed = match args.option("embed-base") {
                Some(raw) if raw.trim().eq_ignore_ascii_case("none") => String::new(),
                Some(raw) => raw.trim().trim_end_matches('/').to_string(),
                None => base.trim_end_matches('/').to_string(),
            };
            if !embed.is_empty() && !valid_base(&embed) {
                eprintln!(
                    "`{embed}` cannot be compiled in: it must be an http:// or https:// URL with no \
                     spaces, `$`, `#`, `&` or `|`. Use --embed-base=none to find the server through DHCP."
                );
                return Ok(exit::FAILURE);
            }
            return build(&root, &arches, &embed, args.option("engine"));
        }

        if args.flag("install") {
            return install_command(&root, &base, &arches, args);
        }

        status(&root, &base);
        Ok(exit::SUCCESS)
    }
}

fn build(root: &Path, arches: &[&str], embed: &str, engine: Option<&str>) -> Result<i32> {
    let context = PathBuf::from("ipxe");
    if !context.join("build.sh").is_file() || !context.join("Containerfile").is_file() {
        eprintln!(
            "`ipxe/build.sh` is not here. pxe:ipxe --build runs from the repository root, where \
             the build scripts and the pinned upstream commit live."
        );
        return Ok(exit::FAILURE);
    }

    let engine = match choose_engine(engine) {
        Ok(engine) => engine,
        Err(e) => {
            eprintln!("{e}");
            return Ok(exit::FAILURE);
        }
    };

    let out = root.join(BUILD_DIR);
    std::fs::create_dir_all(&out).map_err(|e| Error::internal(format!("{}: {e}", out.display())))?;
    let (context, out) = match (absolute(&context), absolute(&out)) {
        (Ok(context), Ok(out)) => (context, out),
        _ => {
            eprintln!("could not work out absolute paths from the current directory");
            return Ok(exit::FAILURE);
        }
    };

    match embed {
        "" => println!("Building iPXE ({}) with no server compiled in, using {engine}.", arches.join(", ")),
        embed => println!("Building iPXE ({}) for {embed}, using {engine}.", arches.join(", ")),
    }

    // A container *build* that exports its own output, not a `run` with the
    // repository mounted: Docker Desktop on Hyper-V refuses to mount a path
    // that is not listed under File Sharing, nothing comes back owned by root,
    // and it works against a remote engine. The context is only `ipxe/`.
    let built = Process::new(&engine)
        .args(["build", "--platform", "linux/amd64", "--target", "output", "-f"])
        .arg(context.join("Containerfile"))
        .arg("--build-arg")
        .arg(format!("ARCHES={}", arches.join(",")))
        .arg("--build-arg")
        .arg(format!("EMBED_BASE={}", if embed.is_empty() { "none" } else { embed }))
        .arg("--output")
        .arg(format!("type=local,dest={}", out.display()))
        .arg(&context)
        .status()
        .map_err(|e| Error::internal(format!("{engine} build: {e}")))?;

    if !built.success() {
        eprintln!("the iPXE build failed; the output above says why. Nothing in service changed.");
        return Ok(exit::FAILURE);
    }

    println!();
    status(root, "");
    Ok(exit::SUCCESS)
}

fn install_command(root: &Path, base: &str, arches: &[&'static str], args: &Arguments) -> Result<i32> {
    let explicit = args.option("arch").is_some_and(|arch| arch.trim() != "all");
    let steps = match plan_install(root, arches, explicit) {
        Ok(steps) if steps.is_empty() => {
            eprintln!("Nothing has been built yet. `pxe:ipxe --build` first.");
            return Ok(exit::FAILURE);
        }
        Ok(steps) => steps,
        Err(e) => {
            eprintln!("{e}");
            return Ok(exit::FAILURE);
        }
    };

    // A loader that comes back to a different server is a rack that boots
    // nothing — or boots somebody else's policy. Refused unless it is meant.
    let mut wrong_server = false;
    for build in builds(root).iter().filter(|build| steps.iter().any(|step| step.arch == build.arch)) {
        let built = build.info.as_ref().and_then(BuildInfo::embed_base);
        if check_base(built, base) == BaseCheck::Differs {
            wrong_server = true;
            eprintln!(
                "The {} build comes back to {}, but this server is {base}.",
                build.arch,
                built.unwrap_or_default()
            );
        }
    }
    if wrong_server && !args.flag("force") {
        eprintln!(
            "Rebuild with `pxe:ipxe --build` (which compiles in PXE_HTTP_BASE), or pass --force if \
             that server is the one you mean."
        );
        return Ok(exit::FAILURE);
    }

    let dry_run = args.flag("dry-run");
    for step in &steps {
        let what = match step.state {
            Installed::Same => "already installed",
            Installed::Absent => "new",
            Installed::Other => "replaces what is there (backed up)",
        };
        println!(
            "  {} {} → {}   {what}",
            if dry_run { "would copy" } else { "copying" },
            step.from.display(),
            step.to.display()
        );
    }
    if dry_run {
        return Ok(exit::SUCCESS);
    }

    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    match install(root, &steps, &stamp) {
        Ok(backups) => {
            if !backups.is_empty() {
                println!(
                    "\nThe previous loaders are in {}. Copy them back to undo.",
                    root.join(BACKUP_DIR).join(&stamp).display()
                );
            }
            println!("Installed. The next machine to boot gets it; nothing needs restarting.");
            Ok(exit::SUCCESS)
        }
        Err(e) => {
            eprintln!(
                "installing failed part-way: {e}. Files already copied are in place and were \
                 backed up first; the rest are untouched. On Windows a file being served over TFTP \
                 at that moment cannot be replaced — try again."
            );
            Ok(exit::FAILURE)
        }
    }
}

fn status(root: &Path, base: &str) {
    let found = builds(root);
    if found.is_empty() {
        println!(
            "No iPXE has been built here. The loaders in {} are whatever was put there by hand.\n\
             `pxe:ipxe --build` compiles this project's own; see ipxe/README.md.",
            root.display()
        );
        return;
    }

    let mut rows = Vec::new();
    for build in &found {
        let info = build.info.clone().unwrap_or_default();
        let server = match (info.embed_base(), base) {
            (None, _) => "found through DHCP".to_string(),
            (Some(built), "") => built.to_string(),
            (Some(built), base) => match check_base(Some(built), base) {
                BaseCheck::Differs => format!("{built}  (NOT this server)"),
                _ => built.to_string(),
            },
        };

        let files: Vec<&str> = info.get("files").map(|files| files.split_whitespace().collect()).unwrap_or_default();
        for file in files {
            let built = build.dir.join(file);
            let size = std::fs::metadata(&built).map(|m| format!("{} KiB", m.len().div_ceil(1024))).unwrap_or_default();
            let installed = INSTALLS
                .iter()
                .find(|(arch, from, _)| *arch == build.arch && *from == file)
                .map(|(_, _, name)| match installed_state(&built, &root.join(name)) {
                    Installed::Same => format!("installed as {name}"),
                    Installed::Other => format!("not installed ({name} is another build)"),
                    Installed::Absent => format!("not installed ({name} absent)"),
                })
                .unwrap_or_else(|| "build only".to_string());

            rows.push(vec![
                format!("{}/{file}", build.arch),
                size,
                info.commit_short().to_string(),
                server.clone(),
                info.get("built_at").unwrap_or("?").to_string(),
                installed,
            ]);
        }
        if build.info.is_none() {
            rows.push(vec![
                build.arch.to_string(),
                String::new(),
                "?".into(),
                "no BUILD-INFO: an interrupted build?".into(),
                String::new(),
                String::new(),
            ]);
        }
    }

    io::table(&["BUILD", "SIZE", "UPSTREAM", "SERVER", "BUILT", "IN SERVICE"], &rows);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxe::facts::OWN_IPXE_USER_CLASS;

    const EMBED: &str = include_str!("../../../../ipxe/embed.ipxe");
    const BUILD_SH: &str = include_str!("../../../../ipxe/build.sh");

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pxe-ipxe-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn built(root: &Path, arch: &str, files: &[(&str, &[u8])], embed_base: &str) {
        let dir = root.join(BUILD_DIR).join(arch);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, bytes) in files {
            std::fs::write(dir.join(file), bytes).unwrap();
        }
        let names: Vec<_> = files.iter().map(|(f, _)| *f).collect();
        std::fs::write(
            dir.join("BUILD-INFO"),
            format!("# comment\narch={arch}\nfiles={}\ncommit=744cdb451ef28bc894df72b6b40fdf1fda04acfc\nembed_base={embed_base}\n", names.join(" ")),
        )
        .unwrap();
    }

    /// `key=value` pairs from the query string of the first line with
    /// `/boot.ipxe?` in it.
    fn query(script: &str) -> BTreeMap<String, String> {
        let line = script.lines().find(|line| line.contains("/boot.ipxe?")).expect("a line asking for /boot.ipxe");
        let query = line.split_once("/boot.ipxe?").unwrap().1;
        let query = query.split_whitespace().next().unwrap();
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_embedded_script_asks_for_everything_the_reflector_would() {
        // The point of asking directly is skipping the reflector's round trip.
        // Skip a parameter and a rule on it silently stops matching for every
        // machine on this build — so the two are held to each other here.
        let reflector = query(&crate::app::http::controllers::boot_controller::reflector("http://x"));
        let embedded = query(EMBED);

        for (key, value) in &reflector {
            let ours = embedded.get(key).unwrap_or_else(|| panic!("embed.ipxe does not send `{key}`"));
            if key == "mac" {
                // The interface that actually got an address, not always net0.
                assert!(ours.ends_with("/mac:hexhyp}"), "mac must be hex-hyphen like the reflector's: {ours}");
            } else {
                assert_eq!(ours, value, "`{key}` is spelled differently from the reflector");
            }
        }
    }

    #[test]
    fn the_embedded_script_identifies_itself_the_way_the_server_checks() {
        assert!(
            EMBED.lines().any(|line| line.trim() == format!("set user-class {OWN_IPXE_USER_CLASS}")),
            "embed.ipxe must send option 77 as `{OWN_IPXE_USER_CLASS}`"
        );
        // Set before the first DHCP, or the first request goes out as stock.
        let class = EMBED.find("set user-class").unwrap();
        let dhcp = EMBED.find("\ndhcp ").unwrap();
        assert!(class < dhcp, "the user class is set after DHCP has already run");
    }

    #[test]
    fn the_embedded_script_never_hands_a_boot_loader_back_to_itself() {
        // Every name `--install` writes is one a DHCP server may be handing
        // out, so every one of them is refused as a boot filename.
        for (_, _, name) in INSTALLS {
            assert!(
                EMBED.contains(&format!("iseq ${{sw-file}} {name} && goto loader-filename")),
                "embed.ipxe would chain `{name}` if DHCP offered it"
            );
        }
    }

    #[test]
    fn every_marker_in_the_embedded_script_is_one_the_build_fills_in() {
        let mut rest = EMBED;
        while let Some(start) = rest.find('@') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('@') else { break };
            let marker = &after[..end];
            if !marker.is_empty() && marker.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                assert!(BUILD_SH.contains(&format!("@{marker}@")), "build.sh never fills in @{marker}@");
                rest = &after[end + 1..];
            } else {
                rest = after;
            }
        }
    }

    #[test]
    fn the_shell_build_and_this_command_agree_on_the_architectures() {
        let line = format!("all_arches=\"{}\"", ARCHES.join(" "));
        assert!(BUILD_SH.contains(&line), "build.sh's architecture list has drifted from ARCHES");
        for arch in ARCHES {
            assert!(BUILD_SH.contains(&format!("        {arch})")), "build.sh has no case for {arch}");
        }
    }

    #[test]
    fn architecture_names_from_the_policy_file_are_understood() {
        assert_eq!(normalise_arch("x64-uefi"), Some("x86_64-efi"));
        assert_eq!(normalise_arch("ARM64-UEFI"), Some("arm64-efi"));
        assert_eq!(normalise_arch("bios"), Some("bios"));
        assert_eq!(normalise_arch("x86-uefi"), Some("i386-efi"));
        assert_eq!(normalise_arch("riscv64"), None);

        assert_eq!(selected_arches(None).unwrap(), ARCHES.to_vec());
        assert_eq!(selected_arches(Some("all")).unwrap(), ARCHES.to_vec());
        assert_eq!(selected_arches(Some("x64-uefi,x86_64-efi,bios")).unwrap(), vec!["x86_64-efi", "bios"]);
        assert!(selected_arches(Some("bios,sparc")).unwrap_err().contains("sparc"));
    }

    #[test]
    fn build_info_reads_what_build_sh_writes() {
        let info = BuildInfo::parse(
            "# Written by ipxe/build.sh.\nproduct=kindling iPXE\ncommit=744cdb451ef28bc894df72b6b40fdf1fda04acfc\n\
             embed_base=\nfiles=ipxe.efi snponly.efi\n",
        );
        assert_eq!(info.get("product"), Some("kindling iPXE"));
        assert_eq!(info.embed_base(), None, "empty means found through DHCP, not an empty URL");
        assert_eq!(info.commit_short(), "744cdb451ef2");

        // Every key it reads is one build.sh writes.
        for key in ["files=", "commit=", "embed_base=", "built_at=", "user_class="] {
            assert!(BUILD_SH.contains(&format!("\n{key}")), "build.sh does not write `{key}`");
        }
    }

    #[test]
    fn a_build_is_checked_against_the_server_it_will_come_back_to() {
        assert_eq!(check_base(Some("http://10.0.1.109:8080"), "http://10.0.1.109:8080/"), BaseCheck::Matches);
        assert_eq!(check_base(Some("HTTP://10.0.1.109:8080/"), "http://10.0.1.109:8080"), BaseCheck::Matches);
        assert_eq!(check_base(None, "http://10.0.1.109:8080"), BaseCheck::Discovers);
        assert_eq!(check_base(Some("http://10.0.0.2:8080"), "http://10.0.1.109:8080"), BaseCheck::Differs);
    }

    #[test]
    fn only_a_url_ipxe_can_use_as_is_is_compiled_in() {
        assert!(valid_base("http://10.0.1.109:8080"));
        assert!(valid_base("https://boot.example.internal"));
        assert!(!valid_base("10.0.1.109:8080"), "no scheme");
        assert!(!valid_base("tftp://10.0.1.109"));
        assert!(!valid_base("http://x/a b"), "a space splits the iPXE command");
        assert!(!valid_base("http://x/${net0/ip}"), "iPXE would expand it");
        assert!(!valid_base("http://"));
    }

    #[test]
    fn installing_backs_up_what_it_replaces_and_skips_what_is_already_there() {
        let root = scratch("install");
        built(&root, "x86_64-efi", &[("ipxe.efi", b"new efi"), ("snponly.efi", b"new snp")], "");
        built(&root, "bios", &[("undionly.kpxe", b"new kpxe")], "");
        std::fs::write(root.join("ipxe.efi"), b"stock efi").unwrap();
        std::fs::write(root.join("undionly.kpxe"), b"new kpxe").unwrap();

        let steps = plan_install(&root, &ARCHES, false).unwrap();
        let state = |name: &str| steps.iter().find(|s| s.to == root.join(name)).map(|s| s.state);
        assert_eq!(state("ipxe.efi"), Some(Installed::Other));
        assert_eq!(state("snponly.efi"), Some(Installed::Absent));
        assert_eq!(state("undionly.kpxe"), Some(Installed::Same));
        assert_eq!(state("ipxe32.efi"), None, "never built, so not part of an --arch=all install");

        let backups = install(&root, &steps, "stamp").unwrap();
        assert_eq!(backups, vec![root.join(BACKUP_DIR).join("stamp").join("ipxe.efi")]);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), b"stock efi");
        assert_eq!(std::fs::read(root.join("ipxe.efi")).unwrap(), b"new efi");
        assert_eq!(std::fs::read(root.join("snponly.efi")).unwrap(), b"new snp");
        assert!(!root.join("ipxe.efi.installing").exists(), "no staging file is left behind");

        // The i386 EFI build is installed under the name the policy expects.
        built(&root, "i386-efi", &[("ipxe.efi", b"ia32"), ("snponly.efi", b"ia32 snp")], "");
        let steps = plan_install(&root, &["i386-efi"], true).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].to, root.join("ipxe32.efi"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn asking_to_install_an_architecture_that_was_never_built_is_an_error() {
        let root = scratch("unbuilt");
        let error = plan_install(&root, &["arm64-efi"], true).unwrap_err();
        assert!(error.contains("--arch=arm64-efi"), "{error}");
        assert!(plan_install(&root, &ARCHES, false).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn builds_are_found_with_or_without_their_build_info() {
        let root = scratch("found");
        built(&root, "arm64-efi", &[("ipxe.efi", b"a")], "http://10.0.1.109:8080");
        std::fs::create_dir_all(root.join(BUILD_DIR).join("bios")).unwrap();

        let found = builds(&root);
        let arches: Vec<_> = found.iter().map(|b| b.arch).collect();
        assert_eq!(arches, vec!["bios", "arm64-efi"]);
        assert!(found[0].info.is_none());
        assert_eq!(found[1].info.as_ref().unwrap().embed_base(), Some("http://10.0.1.109:8080"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
