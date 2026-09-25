import { onBeforeUnmount, reactive } from "vue";

/**
 * The one connection to `/ws/live`, shared by every screen.
 *
 * One rather than one per page, because pages come and go with navigation and
 * a socket per page would reconnect on every click — and a boot event that
 * arrived in the gap would be on nobody's screen. The connection outlives the
 * pages; pages only subscribe and unsubscribe.
 *
 * What arrives is `{ type, data }`: `event` (a boot log row), `host` (a
 * machine as it now is), `host.forgotten`, `policy`, `config`, and `resync`.
 * The shapes are the read API's own, so a page patches its state with the
 * same objects it loaded.
 *
 * `resync` is the one every page must handle, and the answer is always "load
 * again". The server sends it when this browser fell behind and messages were
 * dropped; this module sends it to the pages itself after a reconnect,
 * because whatever happened while the connection was down was not heard
 * either. A page that reloads on `resync` can never be quietly wrong.
 */

/** What the indicator in the header shows. */
export const live = reactive({
  /** `connecting` | `live` | `reconnecting` */
  state: "connecting",
  /** When the current connection opened, or the last one closed. */
  since: null,
  /** Consecutive failed attempts, which is what the backoff is counted in. */
  attempts: 0,
});

const listeners = new Map();

// The browser pings on this interval, and the server drops a connection that
// has said nothing for 90 seconds — see `SILENCE` in `sockets/live.rs`.
const HEARTBEAT_MS = 25_000;
// No message at all — not even the pong — for this long means the connection
// is dead without having said so. A laptop waking from sleep is the usual
// case: the socket still looks open and nothing will ever arrive on it.
const DEAD_AFTER_MS = 60_000;
// Backoff: 0.5s, 1s, 2s … capped. The cap is short because the usual reason
// to be disconnected is somebody restarting this server, and they are looking
// at the screen waiting for it to come back.
const BACKOFF_BASE_MS = 500;
const BACKOFF_CAP_MS = 15_000;

let socket = null;
let heartbeat = null;
let retry = null;
let lastHeard = 0;
let everConnected = false;

function url() {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}/ws/live`;
}

function dispatch(type, data) {
  for (const key of [type, "*"]) {
    for (const fn of listeners.get(key) || []) {
      try {
        fn(data, type);
      } catch (e) {
        // One page's bad handler must not stop the others hearing the message.
        console.error(`live: a handler for \`${type}\` threw`, e);
      }
    }
  }
}

function connect() {
  clearTimeout(retry);
  retry = null;

  let ws;
  try {
    ws = new WebSocket(url());
  } catch {
    scheduleReconnect();
    return;
  }
  socket = ws;

  ws.onopen = () => {
    lastHeard = Date.now();
    live.attempts = 0;
    live.state = "live";
    live.since = new Date();

    clearInterval(heartbeat);
    heartbeat = setInterval(() => {
      if (Date.now() - lastHeard > DEAD_AFTER_MS) {
        // Closing fires `onclose`, which schedules the reconnect.
        ws.close();
        return;
      }
      if (ws.readyState === WebSocket.OPEN) ws.send("ping");
    }, HEARTBEAT_MS);
  };

  ws.onmessage = (message) => {
    lastHeard = Date.now();
    let parsed;
    try {
      parsed = JSON.parse(message.data);
    } catch {
      return;
    }
    if (!parsed?.type) return;

    if (parsed.type === "hello") {
      // A reconnect means a gap nobody heard, so every page loads again. The
      // first connection needs no such thing: the pages have only just loaded.
      if (everConnected) dispatch("resync", { missed: null });
      everConnected = true;
      return;
    }
    if (parsed.type === "pong") return;

    dispatch(parsed.type, parsed.data);
  };

  ws.onclose = () => {
    if (socket !== ws) return;
    socket = null;
    clearInterval(heartbeat);
    if (live.state === "live") live.since = new Date();
    live.state = "reconnecting";
    scheduleReconnect();
  };

  // `onclose` always follows an error, and does the work.
  ws.onerror = () => {};
}

function scheduleReconnect() {
  if (retry) return;
  const delay = Math.min(BACKOFF_CAP_MS, BACKOFF_BASE_MS * 2 ** live.attempts);
  live.attempts += 1;
  // Jittered, so twenty open tabs do not arrive at a restarted server in the
  // same millisecond.
  retry = setTimeout(connect, delay * (0.75 + Math.random() * 0.5));
}

// While the feed is down the screens fall back to what they did before it
// existed: fetching again on a timer. Every page already answers `resync` with
// "load again", so this is the whole of the fallback — and it is what keeps
// the interface useful against a server too old to have `/ws/live`, rather
// than frozen with an amber dot.
const FALLBACK_POLL_MS = 10_000;
setInterval(() => {
  if (live.state !== "live" && document.visibilityState === "visible") {
    dispatch("resync", { missed: null });
  }
}, FALLBACK_POLL_MS);

// A tab brought back to the front should not wait out a long backoff that was
// accumulated while nobody was looking at it.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && live.state === "reconnecting") {
    live.attempts = 0;
    connect();
  }
});

/** Open the connection. Called once, at startup. */
export function startLive() {
  if (!socket && !retry) connect();
}

/**
 * Listen for one type (or `*` for all). Returns the function that stops.
 * Pages should prefer `useLive`, which stops by itself.
 */
export function onLive(type, fn) {
  if (!listeners.has(type)) listeners.set(type, new Set());
  listeners.get(type).add(fn);
  return () => listeners.get(type)?.delete(fn);
}

/**
 * Listen for as long as the calling component is mounted.
 *
 *   useLive({ event: (row) => …, resync: load });
 */
export function useLive(handlers) {
  const stops = Object.entries(handlers).map(([type, fn]) => onLive(type, fn));
  onBeforeUnmount(() => stops.forEach((stop) => stop()));
}

/**
 * A function that runs at most once per `ms`, a moment after it is first asked.
 *
 * What a page wants when forty machines arrive in two seconds and its answer
 * to each is "fetch the counts again": a few fetches, not forty. Not a
 * debounce — a rack that keeps booting is a burst that never ends, and a
 * debounce would wait for it to.
 */
export function coalesced(fn, ms = 500) {
  let timer = null;
  const run = () => {
    if (timer) return;
    timer = setTimeout(() => {
      timer = null;
      fn();
    }, ms);
  };
  run.cancel = () => {
    clearTimeout(timer);
    timer = null;
  };
  return run;
}
