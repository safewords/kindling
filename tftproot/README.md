# The boot root

This directory is served two ways: over TFTP on port 69, and over HTTP under
`/boot/`. They are the same bytes — a loader dropped in here once is reachable
by firmware that can only speak TFTP and by iPXE, which would rather not.

What belongs here:

| File | For |
|---|---|
| `undionly.kpxe` | BIOS machines — the stage-one iPXE binary |
| `ipxe.efi` | 64-bit UEFI machines |
| `ipxe32.efi` | 32-bit UEFI machines |
| `ipxe-arm64.efi` | ARM64 UEFI machines |
| `images/…` | kernels, initrds, ISOs — whatever your profiles point at |

The iPXE binaries come from <https://boot.ipxe.org/>, or from your own build if
you want an embedded script or your own TLS roots. Only the BIOS build is at
that root; the EFI ones are under an architecture directory:

```sh
curl -o undionly.kpxe   https://boot.ipxe.org/undionly.kpxe
curl -o ipxe.efi        https://boot.ipxe.org/x86_64-efi/ipxe.efi
curl -o ipxe32.efi      https://boot.ipxe.org/i386-efi/ipxe.efi
curl -o ipxe-arm64.efi  https://boot.ipxe.org/arm64-efi/ipxe.efi
```

Or build this project's own, which cannot chainload itself in a loop:
`pxe:ipxe --build` writes to `ipxe-build/<arch>/` here, and
`pxe:ipxe --install` copies the builds over the names above, keeping what it
replaced in `ipxe-backup/`. See `ipxe/README.md`.

The names above are the defaults; `[bootloaders]` in `pxe-rules.toml`
overrides them per architecture.

Nothing here is in version control, because these are large binaries that
belong to other projects.

A path is refused if it resolves outside this directory, over either protocol.
