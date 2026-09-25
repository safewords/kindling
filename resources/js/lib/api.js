import { reactive } from "vue";

/**
 * The one place that knows how to talk to this server.
 *
 * Two things live here rather than in the components. The token, because it is
 * attached to every request and a component that forgets is a component that
 * silently reads instead of writing. And the *shape of a failure* — this
 * server answers a refused write with a sentence written for a person, and
 * throwing that sentence is what lets every caller show it without inventing
 * its own wording.
 */

const TOKEN_KEY = "kindling.token";
// Where the token was kept before the project was renamed, so an upgrade does
// not sign everybody out.
const LEGACY_TOKEN_KEY = "safewords-pxe.token";

/** Shared, so every screen sees the same token and the same server facts. */
export const session = reactive({
  token: localStorage.getItem(TOKEN_KEY) || localStorage.getItem(LEGACY_TOKEN_KEY) || "",
  /** Set when a write came back 401/503, so the shell can ask for a token. */
  needsToken: false,
  /** Set when the server says no token is configured at all. */
  tokenNotConfigured: false,
  name: "kindling",
  server: "",
  base: "",
  version: "",
  /**
   * When this tab last sent a write. The live feed announces every policy and
   * configuration change, including this tab's own, and the screen that made
   * the change has already said so — this is how the shell tells the two apart.
   */
  lastWrite: 0,
  /** The policy revision the open screen was loaded at. */
  policyRevision: null,
});

export function setToken(token) {
  session.token = (token || "").trim();
  session.needsToken = false;
  if (session.token) localStorage.setItem(TOKEN_KEY, session.token);
  else localStorage.removeItem(TOKEN_KEY);
}

/** What a failed request throws: a sentence, plus anything structured. */
export class ApiError extends Error {
  constructor(message, { status = 0, problems = [] } = {}) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.problems = problems;
  }
}

async function request(method, path, body) {
  // Validating and testing change nothing, so they are not writes worth
  // suppressing an announcement for.
  if (method !== "GET" && !/\/(validate|test|preview|render)$/.test(path)) session.lastWrite = Date.now();

  const headers = { accept: "application/json", "x-pxe-actor": "web" };
  if (body !== undefined) headers["content-type"] = "application/json";
  // Sent on reads too. Reads do not need it, but a server configured to
  // require one later should not need a second code path.
  if (session.token) headers["x-pxe-token"] = session.token;
  // A policy edit says which revision it was made against, so a change
  // somebody else made in the meantime is refused rather than overwritten.
  if (method !== "GET" && path.startsWith("/api/policy/") && !/(validate|preview|render|templates.*)$/.test(path) && session.policyRevision) {
    headers["x-policy-revision"] = String(session.policyRevision);
  }

  let response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch (cause) {
    // The server is a single process serving the machines in the rack too, so
    // "it is not answering" is worth saying plainly rather than as a
    // TypeError about fetch.
    throw new ApiError("This server is not answering. Is it still running?", { status: 0 });
  }

  if (response.status === 204) return null;

  const text = await response.text();
  let payload = null;
  try {
    payload = text ? JSON.parse(text) : null;
  } catch {
    payload = null;
  }

  if (response.ok) return payload;

  // 401 means the token is wrong or missing; 503 from a write means the
  // server has none configured, which is a different conversation and gets a
  // different message.
  if (response.status === 401) {
    session.needsToken = true;
    session.tokenNotConfigured = false;
  }
  if (response.status === 503 && payload?.error?.includes("PXE_API_TOKEN")) {
    session.needsToken = true;
    session.tokenNotConfigured = true;
  }

  const problems =
    payload?.problems || payload?.details?.problems || (payload?.errors ? Object.values(payload.errors).flat() : []);

  throw new ApiError(
    payload?.message || payload?.error || `The server answered ${response.status}.`,
    { status: response.status, problems },
  );
}

const query = (params) => {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params || {})) {
    if (value !== undefined && value !== null && value !== "") search.set(key, value);
  }
  const text = search.toString();
  return text ? `?${text}` : "";
};

export const api = {
  health: () => request("GET", "/api/health"),

  // --- machines ---
  hosts: (params) => request("GET", `/api/hosts${query(params)}`),
  host: (mac) => request("GET", `/api/hosts/${encodeURIComponent(mac)}`),
  events: (params) => request("GET", `/api/events${query(params)}`),
  facets: () => request("GET", "/api/facets"),

  pin: (mac, profile) => request("POST", `/api/hosts/${encodeURIComponent(mac)}/pin`, { profile }),
  unpin: (mac) => request("DELETE", `/api/hosts/${encodeURIComponent(mac)}/pin`),
  once: (mac, profile) => request("POST", `/api/hosts/${encodeURIComponent(mac)}/once`, { profile }),
  clearOnce: (mac) => request("DELETE", `/api/hosts/${encodeURIComponent(mac)}/once`),
  tag: (mac, add = [], remove = []) =>
    request("POST", `/api/hosts/${encodeURIComponent(mac)}/tags`, { add, remove }),
  forget: (mac) => request("DELETE", `/api/hosts/${encodeURIComponent(mac)}`),
  bulk: (payload) => request("POST", "/api/hosts/bulk", payload),

  // --- policy ---
  policy: () => request("GET", "/api/policy"),
  policySchema: () => request("GET", "/api/policy/schema"),
  validatePolicy: (candidate) => request("POST", "/api/policy/validate", candidate),
  previewPolicy: (candidate) => request("POST", "/api/policy/preview", candidate),
  replacePolicy: (payload) => request("PUT", "/api/policy", payload),
  reloadPolicy: () => request("POST", "/api/rules/reload"),
  exportUrl: (format) => `/api/policy/export?format=${format}`,

  addRule: (rule) => request("POST", "/api/policy/rules", rule),
  putRule: (name, rule) => request("PUT", `/api/policy/rules/${encodeURIComponent(name)}`, rule),
  patchRule: (name, patch) => request("PATCH", `/api/policy/rules/${encodeURIComponent(name)}`, patch),
  removeRule: (name) => request("DELETE", `/api/policy/rules/${encodeURIComponent(name)}`),
  setRuleEnabled: (name, enabled) =>
    request("POST", `/api/policy/rules/${encodeURIComponent(name)}/enabled`, { enabled }),
  reorderRules: (order) => request("POST", "/api/policy/rules/reorder", { order }),

  putProfile: (name, body) => request("PUT", `/api/policy/profiles/${encodeURIComponent(name)}`, body),
  removeProfile: (name) => request("DELETE", `/api/policy/profiles/${encodeURIComponent(name)}`),
  renderProfile: (payload) => request("POST", "/api/policy/profiles/render", payload),

  putSettings: (settings) => request("PUT", "/api/policy/settings", settings),
  setBootloader: (arch, file) =>
    request("PUT", `/api/policy/bootloaders/${encodeURIComponent(arch)}`, { file }),

  revisions: () => request("GET", "/api/policy/revisions"),
  revision: (id) => request("GET", `/api/policy/revisions/${id}`),
  restoreRevision: (id) => request("POST", `/api/policy/revisions/${id}/restore`),

  templates: () => request("GET", "/api/policy/templates"),
  saveTemplate: (template) => request("POST", "/api/policy/templates", template),
  deleteTemplate: (id) => request("DELETE", `/api/policy/templates/${id}`),

  // --- the rule tester ---
  test: (subject) => request("POST", "/api/rules/test", subject),

  // --- configuration ---
  config: () => request("GET", "/api/config"),
  validateConfig: (text) => request("POST", "/api/config/validate", { text }),
  saveConfig: (text) => request("PUT", "/api/config", { text }),
  patchConfig: (changes) => request("PATCH", "/api/config", { changes }),
};
