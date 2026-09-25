/*
 * kindling: where this build writes its output.
 *
 * Upstream's defaults are kept on purpose: the firmware console on EFI (which
 * most server firmware already mirrors to serial and to the BMC), and the BIOS
 * console on pcbios.
 *
 * iPXE's own serial console is NOT switched on. On a machine whose BIOS
 * already redirects its console to serial — which is how IPMI serial-over-LAN
 * works on nearly every server — iPXE writing to the UART as well prints every
 * character twice, and the error message on screen during a failed boot is the
 * thing this project most wants to be readable.
 *
 * For hardware with a serial port and no redirection, uncomment the line
 * below and rebuild. The port and speed are upstream's `config/serial.h`
 * (COM1, 115200 8N1, which is what nearly every console server expects); a
 * `serial.h` beside this file overrides them the same way this one does.
 */

/* #define CONSOLE_SERIAL */
