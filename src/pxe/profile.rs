//! A boot profile: the thing a machine is sent to boot, named once and
//! referred to by rules.
//!
//! A profile can be written four ways, and which one you reach for depends on
//! how much iPXE you feel like writing:
//!
//! - `kind = "local"` — stop network booting, use the disk.
//! - `kernel = …`, `initrd = […]`, `cmdline = …` — the common case, with the
//!   iPXE generated for you.
//! - `script = """…"""` or `script_file = "…"` — verbatim iPXE, for when the
//!   generated version is not enough.
//! - `kind = "menu"` — a list of other profiles, with a timeout and a default.
//!
//! ## Placeholders
//!
//! A profile is written once and used by machines that differ, so it can
//! interpolate: `{{ server }}`, `{{ mac }}`, `{{ arch }}` and the rest below.
//!
//! The braces are doubled **because iPXE's own variables are `${…}`**. If both
//! used the same syntax there would be no way to tell, in a script full of
//! `${net0/mac}`, which expansions happen here and which happen on the
//! machine. They do not collide: `${…}` is passed through untouched.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::facts::ClientFacts;

/// How a profile produces its script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileKind {
    /// Hand control back to the firmware's next boot device.
    Local,
    /// A kernel and initrds, assembled into a script.
    #[default]
    Kernel,
    /// iPXE written out by hand.
    Script,
    /// A menu of other profiles.
    Menu,
    /// Answer nothing at all — the machine is left to its own boot order, and
    /// no file is offered. Different from `local`: that one runs iPXE and
    /// tells it to hand back; this one never sends iPXE in the first place.
    Ignore,
}

/// One entry in a menu profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuEntry {
    /// The profile this entry boots.
    pub profile: String,
    /// What the menu says. Defaults to the target profile's own label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A shortcut key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// A named thing to boot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// What a human calls it. Shown in menus and in the admin UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Stated explicitly, or inferred from which fields are present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProfileKind>,

    // --- kind = "kernel" ---------------------------------------------------
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initrd: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,

    // --- kind = "script" ---------------------------------------------------
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// A path, relative to the rule file's own directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_file: Option<String>,

    // --- kind = "menu" -----------------------------------------------------
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<MenuEntry>,
    /// Seconds before the default entry is taken. Absent means wait forever,
    /// which is the right default for a menu somebody is standing in front of
    /// and the wrong one for a rack — so say a number for a rack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,

    /// Override the stage-one boot file for machines that get this profile,
    /// keyed by architecture label. For the rare machine that should be handed
    /// something other than iPXE — `pxelinux.0`, a vendor's own loader — and
    /// never chainloaded at all.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bootfile: BTreeMap<String, String>,
}

impl Profile {
    /// Which kind this is, stated or inferred.
    pub fn kind(&self) -> ProfileKind {
        if let Some(kind) = self.kind {
            return kind;
        }
        if !self.entries.is_empty() {
            return ProfileKind::Menu;
        }
        if self.script.is_some() || self.script_file.is_some() {
            return ProfileKind::Script;
        }
        ProfileKind::Kernel
    }

    pub fn label_or(&self, name: &str) -> String {
        self.label.clone().unwrap_or_else(|| name.to_string())
    }

    /// Whatever this profile says the firmware should be handed, for its
    /// architecture. `None` means "the usual iPXE binary".
    pub fn bootfile_for(&self, arch_label: &str) -> Option<&str> {
        self.bootfile
            .get(arch_label)
            .or_else(|| self.bootfile.get("default"))
            .map(String::as_str)
    }

    /// Everything wrong with this profile, in the operator's words.
    ///
    /// Checked when the file loads rather than when a machine boots, because
    /// the second one is discovered by a rack that did not come up.
    pub fn problems(&self, name: &str) -> Vec<String> {
        let mut problems = Vec::new();
        match self.kind() {
            ProfileKind::Kernel => {
                if self.kernel.is_none() {
                    problems.push(format!(
                        "profile `{name}` has no `kernel`, no `script` and no `entries`, so there \
                         is nothing for a machine to boot. Add one, or say `kind = \"local\"`."
                    ));
                }
            }
            ProfileKind::Script => {
                if self.script.is_some() && self.script_file.is_some() {
                    problems.push(format!(
                        "profile `{name}` declares both `script` and `script_file`; keep one"
                    ));
                }
                if self.script.is_none() && self.script_file.is_none() {
                    problems.push(format!("profile `{name}` is a script profile with no script"));
                }
            }
            ProfileKind::Menu => {
                if self.entries.is_empty() {
                    problems.push(format!("profile `{name}` is a menu with no entries"));
                }
            }
            ProfileKind::Local | ProfileKind::Ignore => {
                if self.kernel.is_some() || self.script.is_some() {
                    problems.push(format!(
                        "profile `{name}` is `{:?}` but also carries a kernel or a script, which \
                         would never run",
                        self.kind()
                    ));
                }
            }
        }
        problems
    }
}

/// What the placeholders expand to.
#[derive(Debug, Clone)]
pub struct RenderContext<'a> {
    /// The address machines reach this server on, as they should write it.
    pub server: String,
    /// The HTTP base, e.g. `http://10.0.0.2:8080`.
    pub base: String,
    pub facts: &'a ClientFacts,
    /// Every profile, so a menu can read its entries' labels.
    pub profiles: &'a BTreeMap<String, Profile>,
    /// Variables the rules set for this machine: `{{ var.role }}`.
    pub vars: &'a BTreeMap<String, String>,
}

impl RenderContext<'_> {
    fn placeholder(&self, name: &str) -> Option<String> {
        let facts = self.facts;
        Some(match name {
            "server" => self.server.clone(),
            "base" => self.base.clone(),
            // Where the TFTP root is served over HTTP. A profile referring to
            // a kernel next to the boot loader writes `{{boot}}/images/…`
            // rather than repeating the prefix.
            "boot" => format!("{}/{}", self.base, crate::pxe::policy::BOOT_PATH),
            "mac" => facts.mac.to_string(),
            "mac-hyphenless" | "mac_hyphenless" => facts.mac.hyphenless(),
            "oui" => facts.oui.clone(),
            "arch" => facts.arch.label(),
            "arch-code" | "arch_code" => facts.arch.code().to_string(),
            "uuid" => facts.uuid.clone().unwrap_or_default(),
            "hostname" => facts.hostname.clone().unwrap_or_default(),
            "vendor" => facts.vendor.clone().unwrap_or_default(),
            "device-class" | "device_class" => facts.device_class.as_str().to_string(),
            "manufacturer" => facts.manufacturer.clone().unwrap_or_default(),
            "product" => facts.product.clone().unwrap_or_default(),
            "serial" => facts.serial.clone().unwrap_or_default(),
            "asset" => facts.asset.clone().unwrap_or_default(),
            "platform" => facts.platform.clone().unwrap_or_default(),
            "tags" => facts.tags.join(","),
            // A variable no rule set expands to nothing rather than refusing
            // the script: a profile reached from a menu, or by a pin, was not
            // chosen by the rule that sets it, and should still boot.
            other => match other.strip_prefix("var.") {
                Some(key) => self.vars.get(key.trim()).cloned().unwrap_or_default(),
                None => return None,
            },
        })
    }
}

/// Every placeholder a profile may use, for the error message when it uses one
/// that does not exist.
pub const PLACEHOLDERS: &[&str] = &[
    "server",
    "base",
    "boot",
    "mac",
    "mac-hyphenless",
    "oui",
    "arch",
    "arch-code",
    "uuid",
    "hostname",
    "vendor",
    "device-class",
    "manufacturer",
    "product",
    "serial",
    "asset",
    "platform",
    "tags",
    "var.<name>",
];

/// Why a profile could not be turned into a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    UnknownPlaceholder(String),
    UnknownProfile(String),
    Incomplete(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::UnknownPlaceholder(name) => write!(
                f,
                "`{{{{ {name} }}}}` is not a placeholder this server knows. Available: {}. (iPXE's \
                 own `${{…}}` variables are left alone and do not need to be listed here.)",
                PLACEHOLDERS.join(", ")
            ),
            RenderError::UnknownProfile(name) => {
                write!(f, "no profile is called `{name}`")
            }
            RenderError::Incomplete(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for RenderError {}

/// Expand `{{ name }}` placeholders, leaving iPXE's `${…}` alone.
pub fn expand(template: &str, context: &RenderContext<'_>) -> Result<String, RenderError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // An unclosed `{{` is literal text. Refusing here would make a
            // shell script containing one unwritable for no gain.
            out.push_str("{{");
            rest = after;
            continue;
        };
        let name = after[..end].trim();
        match context.placeholder(name) {
            Some(value) => out.push_str(&value),
            None => return Err(RenderError::UnknownPlaceholder(name.to_string())),
        }
        rest = &after[end + 2..];
    }

    out.push_str(rest);
    Ok(out)
}

/// Turn a profile into the iPXE script a machine will run.
pub fn render(
    name: &str,
    profile: &Profile,
    context: &RenderContext<'_>,
) -> Result<String, RenderError> {
    match profile.kind() {
        ProfileKind::Local => Ok(render_local(profile, context)),
        ProfileKind::Ignore => Ok(format!(
            "#!ipxe\n# {name}: this server is deliberately not answering for this machine\nexit 0\n"
        )),
        ProfileKind::Kernel => render_kernel(name, profile, context),
        ProfileKind::Script => {
            let script = profile
                .script
                .as_deref()
                .ok_or_else(|| RenderError::Incomplete(format!("profile `{name}` has no script")))?;
            let expanded = expand(script, context)?;
            Ok(with_shebang(&expanded))
        }
        ProfileKind::Menu => render_menu(name, profile, context),
    }
}

fn render_local(profile: &Profile, context: &RenderContext<'_>) -> String {
    let label = profile.label.as_deref().unwrap_or("local disk");
    // `sanboot` is how a BIOS machine hands control to its first disk. On UEFI
    // there is no equivalent — `exit 0` returns to the firmware, which
    // continues down its own boot order, and that is the correct move there.
    if context.facts.arch.is_uefi() {
        format!(
            "#!ipxe\necho Continuing the firmware boot order ({label})\nexit 0\n"
        )
    } else {
        format!(
            "#!ipxe\necho Booting {label}\nsanboot --no-describe --drive 0x80 || exit 0\n"
        )
    }
}

fn render_kernel(
    name: &str,
    profile: &Profile,
    context: &RenderContext<'_>,
) -> Result<String, RenderError> {
    let kernel = profile.kernel.as_deref().ok_or_else(|| {
        RenderError::Incomplete(format!("profile `{name}` has no `kernel` to boot"))
    })?;

    let mut script = String::from("#!ipxe\n");
    script.push_str(&format!("echo Loading {}\n", profile.label_or(name)));

    let kernel = expand(kernel, context)?;
    match profile.cmdline.as_deref() {
        Some(cmdline) => {
            script.push_str(&format!("kernel {kernel} {}\n", expand(cmdline, context)?))
        }
        None => script.push_str(&format!("kernel {kernel}\n")),
    }

    for initrd in &profile.initrd {
        script.push_str(&format!("initrd {}\n", expand(initrd, context)?));
    }

    // `|| goto failed` rather than a bare `boot`: a failed boot that falls off
    // the end of the script drops to an iPXE prompt in a rack nobody is
    // standing in, where it waits forever.
    script.push_str("boot || goto failed\n:failed\n");
    script.push_str(&format!(
        "echo {} failed to boot; continuing the firmware boot order\nsleep 5\nexit 1\n",
        profile.label_or(name)
    ));
    Ok(script)
}

fn render_menu(
    name: &str,
    profile: &Profile,
    context: &RenderContext<'_>,
) -> Result<String, RenderError> {
    if profile.entries.is_empty() {
        return Err(RenderError::Incomplete(format!("profile `{name}` is a menu with no entries")));
    }

    let mut script = String::from("#!ipxe\n");
    script.push_str(&format!("menu {}\n", profile.label_or(name)));

    for entry in &profile.entries {
        let target = context
            .profiles
            .get(&entry.profile)
            .ok_or_else(|| RenderError::UnknownProfile(entry.profile.clone()))?;
        let label = entry.label.clone().unwrap_or_else(|| target.label_or(&entry.profile));
        match &entry.key {
            Some(key) => {
                script.push_str(&format!("item --key {key} {} {label}\n", entry.profile))
            }
            None => script.push_str(&format!("item {} {label}\n", entry.profile)),
        }
    }

    let default = match &profile.default {
        Some(default) => {
            if !context.profiles.contains_key(default) {
                return Err(RenderError::UnknownProfile(default.clone()));
            }
            default.clone()
        }
        None => profile.entries[0].profile.clone(),
    };

    script.push_str(&format!("choose --default {default}"));
    if let Some(timeout) = profile.timeout {
        script.push_str(&format!(" --timeout {}", timeout * 1000));
    }
    // `|| goto cancelled` catches both the escape key and a timeout with no
    // default, neither of which should leave the machine at a prompt.
    script.push_str(" selected || goto cancelled\n");
    script.push_str(&format!("chain {}/profiles/${{selected}}.ipxe\n", context.base));
    script.push_str(":cancelled\n");
    script.push_str(&format!("chain {}/profiles/{default}.ipxe\n", context.base));
    Ok(script)
}

/// Every iPXE script must start with `#!ipxe` or iPXE will not run it. A
/// hand-written profile that forgot is a machine that does not boot, so the
/// line is added rather than demanded.
fn with_shebang(script: &str) -> String {
    let trimmed = script.trim_start();
    if trimmed.starts_with("#!ipxe") {
        let mut out = trimmed.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    } else {
        format!("#!ipxe\n{}\n", trimmed.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pxe::arch::ClientArch;
    use crate::pxe::facts::Stage;

    fn facts(arch: ClientArch) -> ClientFacts {
        ClientFacts::new("18:66:da:11:22:33".parse().unwrap(), arch, Stage::Ipxe)
            .with_hostname(Some("lab-01".into()))
    }

    static NO_VARS: std::sync::LazyLock<BTreeMap<String, String>> =
        std::sync::LazyLock::new(BTreeMap::new);

    fn context<'a>(
        facts: &'a ClientFacts,
        profiles: &'a BTreeMap<String, Profile>,
    ) -> RenderContext<'a> {
        RenderContext {
            server: "10.0.0.2".into(),
            base: "http://10.0.0.2:8080".into(),
            facts,
            profiles,
            vars: &NO_VARS,
        }
    }

    #[test]
    fn placeholders_expand_and_ipxe_variables_do_not() {
        // The whole reason the syntaxes differ.
        let facts = facts(ClientArch::X64_UEFI);
        let profiles = BTreeMap::new();
        let context = context(&facts, &profiles);

        let rendered =
            expand("kernel {{base}}/vmlinuz mac=${net0/mac} host={{hostname}}", &context).unwrap();
        assert_eq!(rendered, "kernel http://10.0.0.2:8080/vmlinuz mac=${net0/mac} host=lab-01");
    }

    #[test]
    fn a_placeholder_nobody_defined_is_refused_with_the_list() {
        let facts = facts(ClientArch::X64_UEFI);
        let profiles = BTreeMap::new();
        let error = expand("{{ nonsense }}", &context(&facts, &profiles)).unwrap_err();
        assert_eq!(error, RenderError::UnknownPlaceholder("nonsense".into()));
        assert!(error.to_string().contains("server"), "the message lists what is available");
    }

    #[test]
    fn an_unclosed_brace_is_text_rather_than_an_error() {
        let facts = facts(ClientArch::X64_UEFI);
        let profiles = BTreeMap::new();
        let rendered = expand("echo {{ unfinished", &context(&facts, &profiles)).unwrap();
        assert_eq!(rendered, "echo {{ unfinished");
    }

    #[test]
    fn local_means_sanboot_on_bios_and_exit_on_uefi() {
        // There is no `sanboot` equivalent under UEFI; `exit 0` is what hands
        // back to the firmware's own boot order.
        let profiles = BTreeMap::new();
        let profile = Profile { kind: Some(ProfileKind::Local), ..Default::default() };

        let bios = facts(ClientArch::BIOS);
        let script = render("local", &profile, &context(&bios, &profiles)).unwrap();
        assert!(script.contains("sanboot"), "{script}");

        let uefi = facts(ClientArch::X64_UEFI);
        let script = render("local", &profile, &context(&uefi, &profiles)).unwrap();
        assert!(!script.contains("sanboot"), "{script}");
        assert!(script.contains("exit 0"), "{script}");
    }

    #[test]
    fn a_kernel_profile_generates_a_script_that_cannot_fall_through_to_a_prompt() {
        // A bare `boot` that fails leaves an iPXE prompt in an empty room.
        let profiles = BTreeMap::new();
        let facts = facts(ClientArch::X64_UEFI);
        let profile = Profile {
            label: Some("Ubuntu 24.04".into()),
            kernel: Some("{{base}}/images/ubuntu/vmlinuz".into()),
            initrd: vec!["{{base}}/images/ubuntu/initrd".into()],
            cmdline: Some("autoinstall ip=dhcp".into()),
            ..Default::default()
        };

        let script = render("ubuntu", &profile, &context(&facts, &profiles)).unwrap();
        assert!(script.starts_with("#!ipxe\n"));
        assert!(script.contains("kernel http://10.0.0.2:8080/images/ubuntu/vmlinuz autoinstall ip=dhcp"));
        assert!(script.contains("initrd http://10.0.0.2:8080/images/ubuntu/initrd"));
        assert!(script.contains("boot || goto failed"), "{script}");
        assert!(script.contains("exit 1"), "{script}");
    }

    #[test]
    fn a_hand_written_script_gets_the_shebang_it_forgot() {
        // iPXE silently refuses a script without it.
        let profiles = BTreeMap::new();
        let facts = facts(ClientArch::BIOS);
        let profile =
            Profile { script: Some("echo hello\nexit 0".into()), ..Default::default() };
        let script = render("hand", &profile, &context(&facts, &profiles)).unwrap();
        assert!(script.starts_with("#!ipxe\n"), "{script}");
        assert!(script.contains("echo hello"));
    }

    #[test]
    fn a_menu_reads_its_labels_from_the_profiles_it_names() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "local".to_string(),
            Profile {
                label: Some("Boot from disk".into()),
                kind: Some(ProfileKind::Local),
                ..Default::default()
            },
        );
        profiles.insert(
            "ubuntu".to_string(),
            Profile {
                label: Some("Ubuntu 24.04".into()),
                kernel: Some("x".into()),
                ..Default::default()
            },
        );
        let menu = Profile {
            kind: Some(ProfileKind::Menu),
            label: Some("kindling netboot".into()),
            timeout: Some(30),
            default: Some("local".into()),
            entries: vec![
                MenuEntry { profile: "local".into(), label: None, key: Some("l".into()) },
                MenuEntry { profile: "ubuntu".into(), label: Some("Reimage".into()), key: None },
            ],
            ..Default::default()
        };

        let facts = facts(ClientArch::X64_UEFI);
        let script = render("menu", &menu, &context(&facts, &profiles)).unwrap();
        assert!(script.contains("item --key l local Boot from disk"), "{script}");
        assert!(script.contains("item ubuntu Reimage"), "{script}");
        assert!(script.contains("choose --default local --timeout 30000"), "{script}");
        assert!(script.contains(":cancelled"), "a timeout must still go somewhere");
    }

    #[test]
    fn a_menu_naming_a_profile_that_does_not_exist_is_refused() {
        // Fails when the file loads, not when a rack does not come up.
        let profiles = BTreeMap::new();
        let facts = facts(ClientArch::BIOS);
        let menu = Profile {
            kind: Some(ProfileKind::Menu),
            entries: vec![MenuEntry { profile: "typo".into(), label: None, key: None }],
            ..Default::default()
        };
        assert_eq!(
            render("menu", &menu, &context(&facts, &profiles)).unwrap_err(),
            RenderError::UnknownProfile("typo".into())
        );
    }

    #[test]
    fn a_profile_with_nothing_to_boot_says_so() {
        let empty = Profile::default();
        let problems = empty.problems("empty");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("nothing for a machine to boot"), "{problems:?}");
    }

    #[test]
    fn a_profile_can_override_the_boot_file_per_architecture() {
        let mut profile = Profile::default();
        profile.bootfile.insert("bios".into(), "pxelinux.0".into());
        profile.bootfile.insert("default".into(), "ipxe.efi".into());

        assert_eq!(profile.bootfile_for("bios"), Some("pxelinux.0"));
        assert_eq!(profile.bootfile_for("arm64-uefi"), Some("ipxe.efi"));
    }
}
