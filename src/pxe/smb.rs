//! The read-only SMB share — install media for a network-booted Windows Setup.
//!
//! wimboot boots Windows Setup over HTTP, but Setup reads its install image
//! (`install.wim`, several gigabytes) from a disk or an SMB share and from
//! nothing else, and the whole image is too big to hand over in RAM. So the
//! WinPE that wimboot starts maps a share and runs `setup.exe` from it. This is
//! that share, from the same process as everything else a machine boots from,
//! rather than a Samba installed beside it.
//!
//! Off unless `[smb] enabled = true`. When on:
//!
//! - **Read-only.** Every share is [`Access::Read`] for its one account. A
//!   write, rename, delete or attribute change is refused by the server, not
//!   by convention.
//! - **One account, NTLMv2, signed.** WinPE from Windows 11 24H2 on refuses
//!   insecure guest logons and requires signing, so there is no guest mode to
//!   offer; the account is `[smb] user` / `password`, and every session signs
//!   (`require_signing`). An empty password stops `pxe:serve` rather than
//!   serving a share anyone can map.
//! - **Inside the boot root.** A share is a directory under `[tftp] root`,
//!   named relative to it. Opens below it are confined with `cap-std`, so a
//!   symlink in the media cannot reach the rest of the disk.
//!
//! The protocol itself — SMB 2.0.2 to 3.1.1, NTLMv2, signing — is the
//! `smb-server` crate's. This module decides what is shared, to whom, and that
//! it cannot be written.

use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};

use smb_server::{Access, LocalFsBackend, Share, SmbServer};

/// What `[smb]` configures, already checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmbSettings {
    pub bind: IpAddr,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub shares: Vec<ShareSpec>,
}

/// One share: the name a client maps (`\\server\name`) and the directory
/// behind it, relative to the boot root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareSpec {
    pub name: String,
    pub dir: PathBuf,
}

impl SmbSettings {
    /// The settings, or a sentence saying which one is wrong.
    pub fn new(
        bind: &str,
        port: u16,
        user: &str,
        password: &str,
        shares: &str,
    ) -> Result<Self, String> {
        let bind: IpAddr = bind
            .parse()
            .map_err(|_| format!("[smb] bind `{bind}` is not an address"))?;
        if port == 0 {
            return Err("[smb] port must not be 0".into());
        }
        if user.trim().is_empty() {
            return Err(
                "[smb] user is empty: Windows Setup cannot map a share without an account".into(),
            );
        }
        if password.is_empty() {
            // Not a guest share by another name: WinPE refuses guest logons,
            // and an open share of install media is an open share.
            return Err("[smb] password is empty: set one, or turn [smb] off".into());
        }
        let shares = parse_shares(shares)?;
        if shares.is_empty() {
            return Err("[smb] shares names no share: `name=dir`, comma-separated".into());
        }
        Ok(Self {
            bind,
            port,
            user: user.trim().to_string(),
            password: password.to_string(),
            shares,
        })
    }

    /// Build the server, with every share resolved against `root`.
    ///
    /// Fails when a share's directory is missing: a share that maps and then
    /// shows nothing is how Setup fails with a message about drivers.
    pub fn server(&self, root: &Path) -> Result<SmbServer, String> {
        let mut builder = SmbServer::builder()
            .listen(SocketAddr::new(self.bind, self.port))
            .user(&self.user, &self.password)
            .require_signing(true)
            .netbios_name("KINDLING")
            // Finder's extensions are for a share somebody writes to.
            .apple_extensions(false);

        for share in &self.shares {
            let dir = root.join(&share.dir);
            if !dir.is_dir() {
                return Err(format!(
                    "the directory of share `{}` is not there: {}",
                    share.name,
                    dir.display()
                ));
            }
            let backend = LocalFsBackend::new(&dir)
                .map_err(|e| format!("share `{}` ({}): {e}", share.name, dir.display()))?;
            builder = builder.share(
                Share::new(&share.name, backend)
                    .user(&self.user, Access::Read)
                    .remark("kindling, read-only"),
            );
        }

        builder.build().map_err(|e| e.to_string())
    }
}

/// `win11=images/windows11/media, tools=images/tools` into shares.
///
/// A name is what `net use` will be typed with, so letters, digits, `-` and
/// `_`. A directory is relative to the boot root and stays inside it: an
/// absolute path or a `..` is refused here, before anything is opened.
pub fn parse_shares(text: &str) -> Result<Vec<ShareSpec>, String> {
    let mut shares: Vec<ShareSpec> = Vec::new();
    for entry in text
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let Some((name, dir)) = entry.split_once('=') else {
            return Err(format!("[smb] share `{entry}` is not `name=dir`"));
        };
        let (name, dir) = (name.trim(), dir.trim());

        if name.is_empty()
            || name.len() > 80
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "[smb] share name `{name}` must be letters, digits, `-` or `_`"
            ));
        }
        // SMB share names compare without case.
        if shares.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
            return Err(format!("[smb] share `{name}` is named twice"));
        }

        let path = Path::new(dir);
        if dir.is_empty()
            || !path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(format!(
                "[smb] share `{name}`: `{dir}` must be a directory under the boot root, without `..`"
            ));
        }

        shares.push(ShareSpec {
            name: name.to_string(),
            dir: path.to_path_buf(),
        });
    }
    Ok(shares)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(shares: &str) -> Result<SmbSettings, String> {
        SmbSettings::new("0.0.0.0", 445, "install", "not-a-real-password", shares)
    }

    #[test]
    fn shares_are_names_and_directories_under_the_root() {
        let parsed = parse_shares(" win11 = images/windows11/media ,tools=tools,").unwrap();
        assert_eq!(
            parsed,
            vec![
                ShareSpec {
                    name: "win11".into(),
                    dir: "images/windows11/media".into()
                },
                ShareSpec {
                    name: "tools".into(),
                    dir: "tools".into()
                },
            ]
        );
    }

    #[test]
    fn a_share_cannot_point_outside_the_boot_root() {
        for hostile in [
            "x=/etc",
            "x=../storage",
            "x=images/../../etc",
            "x=./images",
            "x=",
        ] {
            assert!(parse_shares(hostile).is_err(), "{hostile} was accepted");
        }
    }

    #[test]
    fn a_share_name_is_something_net_use_can_type() {
        for bad in [
            "=dir",
            "a b=dir",
            "a$=dir",
            "win\\11=dir",
            "a/b=dir",
            "nameonly",
        ] {
            assert!(parse_shares(bad).is_err(), "{bad} was accepted");
        }
        assert!(
            parse_shares("Win11=a,win11=b").is_err(),
            "names compare without case"
        );
    }

    #[test]
    fn there_is_no_open_share() {
        assert!(SmbSettings::new("0.0.0.0", 445, "install", "", "w=d").is_err());
        assert!(SmbSettings::new("0.0.0.0", 445, " ", "pw", "w=d").is_err());
        assert!(settings("").is_err(), "no shares");
        assert!(SmbSettings::new("not-an-ip", 445, "install", "pw", "w=d").is_err());
    }

    #[test]
    fn a_missing_directory_stops_the_server_from_being_built() {
        let root = std::env::temp_dir().join(format!("kindling-smb-{}", std::process::id()));
        std::fs::create_dir_all(root.join("there")).unwrap();

        assert!(settings("w=there").unwrap().server(&root).is_ok());
        let Err(error) = settings("w=missing").unwrap().server(&root) else {
            panic!("a share over a missing directory was built");
        };
        assert!(error.contains("is not there"), "{error}");
    }
}
