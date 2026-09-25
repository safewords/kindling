# This project's iPXE

Upstream iPXE, pinned, with this project's configuration and an embedded
script — built for every architecture the server hands out:

| Build | For | Installed as |
|---|---|---|
| `bios/undionly.kpxe` | BIOS machines, over the NIC's own PXE stack | `undionly.kpxe` |
| `x86_64-efi/ipxe.efi` | 64-bit UEFI, iPXE's own NIC drivers | `ipxe.efi` |
| `x86_64-efi/snponly.efi` | 64-bit UEFI, the firmware's network driver | `snponly.efi` |
| `i386-efi/ipxe.efi` | 32-bit UEFI | `ipxe32.efi` |
| `i386-efi/snponly.efi` | 32-bit UEFI, firmware driver | (stays under `ipxe-build/`) |
| `arm64-efi/ipxe.efi` | ARM64 UEFI, cross-compiled | `ipxe-arm64.efi` |
| `arm64-efi/snponly.efi` | ARM64 UEFI, firmware driver | (stays under `ipxe-build/`) |

It is not a rewrite. iPXE is a TCP/IP stack, an HTTP and TLS client and a
driver for every NIC anybody has shipped; on BIOS it runs in real mode. None
of that is this project's problem to solve again. What this project *does*
need is for the loader to stop taking orders from the DHCP boot filename,
and that is configuration and one script.

## Why

Stock iPXE, started with no embedded script, does DHCP and boots whatever
the "boot filename" in the reply says. The server here recognises iPXE and
answers it with a script instead of a loader, which is what stops the loop
— **when the server is the one answering.** It is not always:

- A router's "Network Boot" option (UniFi's, for one) sets the boot filename
  for every client on the network, iPXE included. Tell it `ipxe.efi` and
  iPXE is told `ipxe.efi`, loads itself, and asks again, for ever. Every
  exchange along the way looks correct.
- A relay that strips option 77, or a second DHCP server that answers
  first, has the same effect.

`tftproot/autoexec.ipxe` works around that for stock EFI builds. This build
removes the cause: the script compiled into it never follows a boot filename
that names a loader, and does not need one at all when it knows where the
server is.

## What the embedded script does

[`embed.ipxe`](embed.ipxe), in order:

1. **Identifies itself.** Option 77 is sent as `iPXE-kindling`. It still
   starts with `iPXE`, so every existing check — this server's, and the
   prefix match dnsmasq and ISC configurations use — still sees iPXE. The
   suffix is an extra signal: `pxe:test --user-class=iPXE-kindling` shows
   `ipxe true (this project's build)`, and rules can match it with
   `user_class = ["iPXE-kindling"]`.
2. **DHCP on every interface in turn**, `net0`, `net1`, … with retries on
   each, rather than giving up on the machine when the first port is on the
   wrong network.
3. **Finds the server**, first to answer wins:
   1. the base URL compiled in at build time (`--embed-base`);
   2. a proxy DHCP server's `next-server` — this server's own proxy;
   3. the DHCP server's `next-server` — where a router's "Network Boot"
      option points;
   4. the boot filename, **only** if it is not a loader. iPXE's script
      language has no prefix test, so "is it an `http(s)://` script" is
      approximated: every loader name this project installs is refused by
      name, and a bare name like `boot/custom.efi` cannot be fetched from an
      embedded script at all (it has no URI to be relative to), so it fails
      rather than resolving against a TFTP server.
4. **Asks directly** for `/boot.ipxe?mac=…&product=…&serial=…` — the same
   parameters the server's reflector would have asked for, from the
   interface that actually got an address — so the decision is made on the
   first HTTP request instead of the second. A unit test holds the two lists
   to each other.
5. **Takes the answer as final.** Once a server has returned a script, its
   outcome is not second-guessed: a `local` profile ends with `exit 0` and
   iPXE hands control back to the firmware; a machine told "no" does not go
   looking for a server that says yes.
6. **Fails visibly.** If nothing answers, it says what it tried, counts
   down ten seconds and exits to the firmware's boot order — any key drops
   into the iPXE shell instead, which is where `ping`, `nslookup` and
   `ifstat` are.

That behaviour was run end to end against iPXE's Linux userspace build
(`bin-x86_64-linux`, networked through slirp): the direct path, the fallback
from a dead baked-in URL to `next-server`, a boot filename of `ipxe.efi`
being refused, and the countdown.

## Configuration

[`config/local/`](config/local) is copied over upstream's `src/config/local/`
before every build:

- `general.h` — HTTPS, and the shell commands someone at a broken machine
  wants: `ping`, `nslookup`, `ntp` (HTTPS needs a clock), `reboot`,
  `poweroff`, `imgtrust`, `params`, `vcreate`, `nstat`, `console`. Chosen to
  keep `undionly.kpxe` far inside the BIOS base-memory budget: it builds to
  about 107 KiB.
- `branding.h` — the banner says `kindling iPXE`, so a person at a
  console can tell which iPXE a machine is running.
- `console.h` — upstream's consoles, and why iPXE's own serial console is
  left off (it doubles every character behind IPMI serial-over-LAN).

The upstream commit is pinned in [`UPSTREAM`](UPSTREAM). Moving it is a
one-line change that shows up in review; the build fetches exactly that
commit and nothing else. There are no patches to upstream — see
[`patches/`](patches/README.md) for how one would be added and why the aim
is to never need one.

## Building

Everything compiles inside the container [`Containerfile`](Containerfile)
describes (Debian, gcc, binutils, the aarch64 cross compiler, liblzma), so
every host — a Windows laptop, a Mac, CI — gets the same toolchain. It needs
Docker or Podman, running.

```sh
cargo run -- pxe:ipxe --build                       # every architecture, for PXE_HTTP_BASE
cargo run -- pxe:ipxe --build --arch=x64-uefi,bios  # just these
cargo run -- pxe:ipxe --build --embed-base=none     # find the server through DHCP
cargo run -- pxe:ipxe                               # what is built, what is installed
```

Or without the Rust app:

```sh
ipxe/build.sh --embed-base=http://10.0.0.2:8080 [--arch=…]
ipxe/build.sh --native …        # a Linux host with the toolchain installed
```

```powershell
.\ipxe\build.ps1 -EmbedBase http://10.0.0.2:8080 [-Arch x86_64-efi,bios]
```

It is a container *build* with the binaries exported by BuildKit
(`--output type=local`), not a `run` with the repository mounted. That is
deliberate: Docker Desktop on Hyper-V refuses to mount any path not listed
under File Sharing, a mount would hand files back owned by root on Linux, and
a build works the same against a remote engine. The upstream checkout and
its object files live in a BuildKit cache mount, so the second build is
incremental. A full build of all seven binaries takes about a minute on a
fast machine.

Outputs land in `tftproot/ipxe-build/<arch>/`, each with a `BUILD-INFO`
(the commit, the URL compiled in, when) and `SHA256SUMS`. **Nothing a
machine is offered changes.**

### Whether to compile a URL in

- **With one** (`pxe:ipxe --build`, which uses `PXE_HTTP_BASE`): the
  machine goes straight to this server whatever DHCP says. Strongest
  guarantee; the binary has to be rebuilt if the server moves. `pxe:doctor`
  warns when a build's URL and `PXE_HTTP_BASE` disagree, and `--install`
  refuses such a build without `--force`.
- **Without** (`--embed-base=none`): the machine finds the server through
  `next-server`. Survives the server moving; relies on DHCP pointing at it,
  which is true whenever this server's proxy or a router's "Network Boot"
  option is how the machine got here.

HTTPS bases work, against upstream's trusted roots. For a private CA pass
`IPXE_MAKE_ARGS="TRUST=/work/ipxe/ca.pem"` with the certificate placed in
this directory.

## Trying one machine first

`tftproot/ipxe-build/` is inside the boot root, so a build is reachable over
TFTP and HTTP before it is installed. To put one machine through it, copy the
profile that machine would normally get, name the build as its loader, and
pin the machine to the copy:

```toml
[profiles.ubuntu-2404-new-ipxe]
label    = "Ubuntu 24.04, through this project's iPXE"
bootfile = { x64-uefi = "ipxe-build/x86_64-efi/ipxe.efi", bios = "ipxe-build/bios/undionly.kpxe" }
kernel   = "{{boot}}/images/ubuntu-24.04/vmlinuz"      # the rest as in ubuntu-2404
initrd   = ["{{boot}}/images/ubuntu-24.04/initrd"]
cmdline  = "autoinstall ds=nocloud-net;s={{base}}/seed/{{mac-hyphenless}}/"
```

```sh
cargo run -- pxe:pin --mac=… --profile=ubuntu-2404-new-ipxe
```

A profile's `bootfile` is offered even to iPXE, which is exactly the loop a
stock build would fall into. This one does not follow it: it goes to the
server it was built for, gets the profile's kernel, and the loop never
starts — which makes it a fair test of the thing it was built to fix.
`pxe:pin --mac=… --clear` when done.

## Installing

```sh
cargo run -- pxe:ipxe --install --dry-run   # what would change
cargo run -- pxe:ipxe --install             # every architecture that is built
cargo run -- pxe:ipxe --install --arch=x64-uefi
```

Each file is written beside its destination and renamed over it, so a TFTP
transfer that starts mid-install gets the old loader or the new one, never
half of each. Whatever is replaced is copied to
`tftproot/ipxe-backup/<timestamp>/` first; copying it back is the undo.
Nothing needs restarting: the next machine to boot gets the new loader.

Once installed, `tftproot/autoexec.ipxe` is redundant — this build carries
the same logic compiled in, and more of it.

## CI

[`.github/workflows/ipxe.yml`](../.github/workflows/ipxe.yml) builds each
architecture in parallel with the same `build.sh` and uploads
`ipxe-<arch>` artifacts. It compiles a URL in only when the repository
variable `PXE_EMBED_BASE` is set, or a manual run supplies one.
