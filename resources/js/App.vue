<script setup>
import { RouterLink, RouterView, useRoute } from "vue-router";
import { computed, ref } from "vue";
import { session, setToken } from "./lib/api.js";
import { live, useLive } from "./lib/live.js";
import { toasts, dismiss, notify } from "./lib/toast.js";

const route = useRoute();
const tokenInput = ref("");

const NAV = [
  { to: "/", label: "Overview", exact: true },
  { to: "/devices", label: "Machines" },
  { to: "/policy", label: "Policy" },
  { to: "/events", label: "Boot log" },
  { to: "/config", label: "Configuration" },
];

const active = (item) =>
  item.exact ? route.path === item.to : route.path.startsWith(item.to);

const saveToken = () => {
  setToken(tokenInput.value);
  tokenInput.value = "";
};

// A change to the policy or the configuration is announced here, once, on
// whatever screen is open — because the person who needs to hear that the
// running policy just changed is rarely the one looking at the policy editor.
// This tab's own writes are skipped: the screen that made them has said so.
const OWN_WRITE_MS = 3000;
const ownWrite = () => Date.now() - session.lastWrite < OWN_WRITE_MS;

useLive({
  policy: (change) => {
    if (!change.reloaded) {
      // Shown even for this tab's own write. A save that did not take is the
      // one message that must not be deduplicated away.
      notify.bad("The stored policy did not load. The previous policy is still running.", change.error);
    } else if (!ownWrite()) {
      notify.info(`Policy reloaded: ${change.rules} rule${change.rules === 1 ? "" : "s"}, ${change.profiles} profile${change.profiles === 1 ? "" : "s"}.`);
    }
  },
  config: (change) => {
    if (ownWrite()) return;
    notify.info(
      "The configuration was edited. It takes effect when the server restarts.",
      (change.changed || []).join(", "),
    );
  },
});

const LIVE_LABEL = {
  live: "live",
  connecting: "connecting",
  reconnecting: "reconnecting",
};

const tone = computed(() => ({
  ok: "border-emerald-400/40 bg-emerald-400/10 text-emerald-200",
  info: "border-sky-400/40 bg-sky-400/10 text-sky-200",
  bad: "border-rose-400/40 bg-rose-400/10 text-rose-200",
}));
</script>

<template>
  <div class="min-h-full">
    <header class="sticky top-0 z-30 border-b border-slate-800 bg-slate-950/90 backdrop-blur">
      <div class="mx-auto flex max-w-[1600px] flex-wrap items-center gap-x-8 gap-y-2 px-4">
        <RouterLink to="/" class="flex shrink-0 items-baseline gap-1.5 py-3 text-[15px] font-semibold tracking-tight">
          kindling
        </RouterLink>

        <nav class="flex flex-1 flex-wrap gap-x-1">
          <RouterLink
            v-for="item in NAV"
            :key="item.to"
            :to="item.to"
            class="border-b-2 px-3 py-3 text-sm transition-colors"
            :class="
              active(item)
                ? 'border-sky-400 text-slate-100'
                : 'border-transparent text-slate-400 hover:border-slate-700 hover:text-slate-200'
            "
          >
            {{ item.label }}
          </RouterLink>
        </nav>

        <!-- The address machines are told to come back to. It is on every
             screen because it is the one setting whose being wrong explains
             every other symptom. -->
        <div class="hidden items-center gap-2 py-3 text-xs text-slate-500 lg:flex">
          <span class="text-slate-600">machines reach</span>
          <code class="font-mono text-slate-400">{{ session.server || "—" }}</code>
        </div>

        <!-- Whether what is on screen is current. Said out loud because a
             screen that has silently stopped updating looks exactly like a
             network where nothing is booting. -->
        <div
          class="flex items-center gap-1.5 py-3 text-xs"
          :class="live.state === 'live' ? 'text-emerald-400' : 'text-amber-300'"
          :title="
            live.state === 'live'
              ? 'Changes appear as they happen.'
              : 'Not connected to the live feed; what is on screen may be out of date. Retrying.'
          "
        >
          <span class="relative flex h-2 w-2">
            <span
              v-if="live.state === 'live'"
              class="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-40"
            ></span>
            <span
              class="relative inline-flex h-2 w-2 rounded-full"
              :class="live.state === 'live' ? 'bg-emerald-400' : 'bg-amber-300'"
            ></span>
          </span>
          {{ LIVE_LABEL[live.state] }}
        </div>
      </div>
    </header>

    <!-- Writing is guarded by a token, so if the server has refused one the
         whole interface says so once, here, rather than each button failing
         on its own. -->
    <div
      v-if="session.needsToken"
      class="border-b border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-100"
    >
      <div class="mx-auto flex max-w-[1600px] flex-wrap items-center gap-3">
        <template v-if="session.tokenNotConfigured">
          <div class="flex-1">
            <b>No API token is configured on this server</b>, so everything that changes
            what a machine boots is closed. Set <code class="font-mono">PXE_API_TOKEN</code>
            in <code class="font-mono">.env</code> and restart. Reading is unaffected.
          </div>
        </template>
        <template v-else>
          <div class="flex-1">
            That write needs the server's API token.
          </div>
          <input
            v-model="tokenInput"
            type="password"
            placeholder="PXE_API_TOKEN"
            class="w-72 rounded-md border border-amber-500/30 bg-slate-950/60 px-3 py-1.5 font-mono text-xs text-amber-50 outline-none placeholder:text-amber-200/30 focus:border-amber-400"
            @keyup.enter="saveToken"
          />
          <button
            class="rounded-md bg-amber-400 px-3 py-1.5 text-xs font-semibold text-slate-950 hover:bg-amber-300"
            @click="saveToken"
          >
            Use it
          </button>
        </template>
      </div>
    </div>

    <main class="mx-auto max-w-[1600px] px-4 py-6">
      <RouterView />
    </main>

    <div class="pointer-events-none fixed bottom-4 right-4 z-50 flex w-96 max-w-[calc(100vw-2rem)] flex-col gap-2">
      <div
        v-for="toast in toasts"
        :key="toast.id"
        class="rise pointer-events-auto rounded-lg border px-4 py-3 text-sm shadow-lg shadow-black/40"
        :class="tone[toast.tone]"
      >
        <div class="flex items-start gap-3">
          <div class="flex-1">
            <div>{{ toast.message }}</div>
            <pre
              v-if="toast.detail"
              class="mt-2 max-h-40 overflow-auto whitespace-pre-wrap font-mono text-[11px] opacity-80"
            >{{ toast.detail }}</pre>
          </div>
          <button class="shrink-0 opacity-60 hover:opacity-100" @click="dismiss(toast.id)">✕</button>
        </div>
      </div>
    </div>
  </div>
</template>
