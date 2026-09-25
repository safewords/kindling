/*
 * kindling: what this project's iPXE can do beyond upstream's defaults.
 *
 * Copied over `src/config/local/` in the pinned upstream tree before every
 * build, so it is read after `config/general.h` has set its defaults: a
 * `#define` here switches a feature on whatever upstream decided, and an
 * `#undef` switches one off.
 *
 * Everything here is on for every architecture, including `undionly.kpxe`.
 * That one is the budget: a BIOS PXE ROM loads it into base memory, and the
 * practical ceiling is somewhere around 500 KiB on older machines. HTTPS is
 * the expensive entry (the TLS stack and its ciphers, roughly 100 KiB); the
 * commands are a few KiB each. What is deliberately NOT here — the
 * framebuffer console, Wi-Fi, the menu-heavy extras — is left out to keep the
 * BIOS build comfortably inside that.
 */

/* Fetch over TLS. A profile can point at https:// kernels and ISOs, and the
 * embedded script can be built with an https:// base. The roots trusted are
 * upstream's (ipxe.org's cross-signed CA) unless the build passes TRUST=. */
#define DOWNLOAD_PROTO_HTTPS

/* Diagnosis from the iPXE shell — the one place anybody is when a boot has
 * gone wrong on a machine with no operating system on it yet. */
#define PING_CMD		/* is the server reachable at all */
#define NSLOOKUP_CMD		/* does the name in a profile resolve */
#define NEIGHBOUR_CMD		/* the ARP table: who answered */
#define NTP_CMD			/* HTTPS needs a clock; broken RTCs are common */

/* Getting out of a bad state without walking to the rack. */
#define REBOOT_CMD
#define POWEROFF_CMD

/* `imgtrust` / `imgverify`: refuse to run anything that is not signed, for
 * the sites that want the boot chain to prove itself. */
#define IMAGE_TRUST_CMD

/* `params` / `param`: form parameters for `chain --post` (a machine reporting
 * back to the server over POST rather than an ever-longer query string). */
#define PARAM_CMD

/* 802.1Q, for boot networks that are a tagged VLAN on a trunk port. */
#define VLAN_CMD

/* `console`: switch resolution / console at runtime. Cheap, and useful on
 * EFI machines whose firmware leaves the screen in something unreadable. */
#define CONSOLE_CMD
