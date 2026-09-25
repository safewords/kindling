import { reactive } from "vue";

/**
 * The notices that appear in the corner.
 *
 * A shared list rather than a per-component one, because the thing worth
 * telling somebody about — "the policy reloaded", "that machine has not been
 * seen" — is usually produced by a component that is about to be replaced by
 * the navigation that follows.
 */
export const toasts = reactive([]);

let nextId = 1;

function push(tone, message, detail) {
  const id = nextId++;
  toasts.push({ id, tone, message, detail });

  // Failures stay until they are dismissed. Somebody who looked away for six
  // seconds should not have to guess what went wrong.
  if (tone !== "bad") {
    setTimeout(() => dismiss(id), 4200);
  }
  return id;
}

export function dismiss(id) {
  const index = toasts.findIndex((toast) => toast.id === id);
  if (index !== -1) toasts.splice(index, 1);
}

export const notify = {
  ok: (message, detail) => push("ok", message, detail),
  info: (message, detail) => push("info", message, detail),
  bad: (message, detail) => push("bad", message, detail),

  /**
   * Report a thrown `ApiError` without every call site rewriting the wording.
   * The server writes its refusals for a person to read; this shows them.
   */
  error: (e) => push("bad", e?.message || "Something went wrong.", (e?.problems || []).join("\n")),
};
