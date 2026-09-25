/** Small shared formatting, so two screens never disagree about a date. */

import { ref } from "vue";

/**
 * A clock the relative times read, so "just now" becomes "2 minutes ago" on
 * its own. The screens used to re-render by polling; now that changes are
 * pushed, a quiet network would otherwise leave every timestamp frozen at
 * whatever it said when the last message arrived.
 */
const clock = ref(Date.now());
setInterval(() => (clock.value = Date.now()), 15_000);

const RELATIVE = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

const STEPS = [
  ["second", 60],
  ["minute", 60],
  ["hour", 24],
  ["day", 7],
  ["week", 4.348],
  ["month", 12],
  ["year", Infinity],
];

/**
 * "4 minutes ago".
 *
 * Relative rather than absolute because every question on these screens is
 * about recency — did this machine boot *just now*, is this policy the one
 * running — and nobody converts 10:45:31Z into that in their head.
 */
export function ago(iso) {
  if (!iso) return "—";
  const then = new Date(iso);
  if (Number.isNaN(then.getTime())) return "—";

  let delta = (then.getTime() - Math.max(clock.value, Date.now())) / 1000;
  for (const [unit, size] of STEPS) {
    if (Math.abs(delta) < size) return RELATIVE.format(Math.round(delta), unit);
    delta /= size;
  }
  return then.toLocaleDateString();
}

/** The full timestamp, for a tooltip beside the relative one. */
export function exact(iso) {
  if (!iso) return "";
  const then = new Date(iso);
  return Number.isNaN(then.getTime()) ? "" : then.toLocaleString();
}

export function bytes(n) {
  if (!Number.isFinite(n)) return "—";
  const units = ["B", "KB", "MB", "GB"];
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 && unit > 0 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** Title-case a kebab or snake identifier for a heading. */
export function humanise(text) {
  return String(text || "")
    .replace(/[-_]/g, " ")
    .replace(/^\w/, (c) => c.toUpperCase());
}

/**
 * The colour a boot event is drawn in.
 *
 * `refused` is amber rather than red on purpose: this server refusing to
 * answer is very often the *correct* outcome — a switch it was told to leave
 * alone — and painting that like an error trains people to ignore red.
 */
export function eventTone(kind) {
  switch (kind) {
    case "offer":
      return "text-sky-300";
    case "script":
      return "text-emerald-300";
    case "tftp":
    case "http":
      return "text-slate-300";
    case "refused":
      return "text-amber-300";
    default:
      return "text-slate-400";
  }
}

export function deviceTone(deviceClass) {
  switch (deviceClass) {
    case "virtual":
      return "text-violet-300 border-violet-400/30 bg-violet-400/10";
    case "physical":
      return "text-sky-300 border-sky-400/30 bg-sky-400/10";
    case "sbc":
      return "text-emerald-300 border-emerald-400/30 bg-emerald-400/10";
    case "network":
      return "text-amber-300 border-amber-400/30 bg-amber-400/10";
    default:
      return "text-slate-400 border-slate-600/40 bg-slate-500/10";
  }
}
