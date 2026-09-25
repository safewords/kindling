<script setup>
import { onMounted, ref } from "vue";
import { RouterLink } from "vue-router";
import { api, session } from "../lib/api.js";
import { ago, exact, eventTone } from "../lib/format.js";
import { coalesced, useLive } from "../lib/live.js";
import { notify } from "../lib/toast.js";

/** How many rows "recent activity" shows. */
const RECENT = 12;

const health = ref(null);
const hosts = ref({ total: 0, data: [] });
const events = ref([]);
const loading = ref(true);

async function load() {
  try {
    const [h, inventory, log] = await Promise.all([
      api.health(),
      api.hosts({ per_page: 1 }),
      api.events({ limit: RECENT }),
    ]);
    health.value = h;
    hosts.value = inventory;
    events.value = log.data;
  } catch (e) {
    notify.error(e);
  } finally {
    loading.value = false;
  }
}

/**
 * The counters, fetched again rather than counted here.
 *
 * A `host` message does not say whether the machine is new, and a counter
 * kept in the browser would drift the first time a message was missed. So a
 * change is a prompt to ask the server — coalesced, so a rack of forty
 * arriving at once costs a handful of requests rather than forty.
 */
const recount = coalesced(async () => {
  try {
    const [h, inventory] = await Promise.all([api.health(), api.hosts({ per_page: 1 })]);
    health.value = h;
    hosts.value = inventory;
  } catch {
    // Quiet: the connection indicator is already saying the server is gone.
  }
}, 750);

useLive({
  event: (row) => {
    if (events.value.some((e) => e.id === row.id)) return;
    events.value = [row, ...events.value].slice(0, RECENT);
  },
  host: recount,
  "host.forgotten": recount,
  policy: recount,
  resync: load,
});

onMounted(load);
</script>

<template>
  <div class="space-y-6">
    <!-- The one banner that is genuinely urgent: the file on disk is not what
         is running, so somebody's edit is not live. -->
    <div
      v-if="health?.rules?.last_reload_error"
      class="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm"
    >
      <div class="font-semibold text-rose-200">
        The policy file on disk does not load, so what is running is older than the file.
      </div>
      <pre class="mt-2 whitespace-pre-wrap font-mono text-xs text-rose-200/80">{{
        health.rules.last_reload_error.error
      }}</pre>
      <RouterLink to="/policy" class="mt-2 inline-block text-xs text-rose-300 underline">
        Open the policy editor
      </RouterLink>
    </div>

    <div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      <RouterLink
        to="/devices"
        class="rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3 transition-colors hover:border-slate-700"
      >
        <div class="text-2xl font-semibold tabular-nums">{{ hosts.total }}</div>
        <div class="mt-0.5 text-xs uppercase tracking-wide text-slate-500">machines known</div>
      </RouterLink>

      <RouterLink
        to="/policy"
        class="rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3 transition-colors hover:border-slate-700"
      >
        <div class="text-2xl font-semibold tabular-nums">{{ health?.rules?.rules ?? "—" }}</div>
        <div class="mt-0.5 text-xs uppercase tracking-wide text-slate-500">rules in force</div>
      </RouterLink>

      <RouterLink
        to="/policy"
        class="rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3 transition-colors hover:border-slate-700"
      >
        <div class="text-2xl font-semibold tabular-nums">{{ health?.rules?.profiles ?? "—" }}</div>
        <div class="mt-0.5 text-xs uppercase tracking-wide text-slate-500">profiles</div>
      </RouterLink>

      <div class="rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3">
        <div
          class="text-2xl font-semibold"
          :class="health?.inventory?.status === 'ok' ? 'text-emerald-400' : 'text-rose-400'"
        >
          {{ health?.inventory?.status === "ok" ? "healthy" : "degraded" }}
        </div>
        <div class="mt-0.5 text-xs uppercase tracking-wide text-slate-500">
          {{ health?.inventory?.status === "ok" ? "inventory reachable" : "inventory unreachable" }}
        </div>
      </div>
    </div>

    <!-- A database this server cannot reach does not stop machines booting;
         it stops it *explaining* them. Saying which is the difference between
         a page at 3am and a note in the morning. -->
    <div
      v-if="health && health.inventory?.status !== 'ok'"
      class="rounded-lg border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm text-amber-100"
    >
      <b>The inventory is unreachable.</b> Machines still boot — policy does not depend on it —
      but nothing is being recorded and overrides are not being read.
      <span v-if="health.inventory_error" class="font-mono text-xs opacity-80">
        {{ health.inventory_error }}
      </span>
    </div>

    <div class="grid gap-6 lg:grid-cols-3">
      <section class="lg:col-span-2">
        <div class="mb-2 flex items-baseline justify-between">
          <h2 class="text-xs font-semibold uppercase tracking-wider text-slate-500">
            Recent activity
          </h2>
          <RouterLink to="/events" class="text-xs text-slate-500 hover:text-slate-300">
            the whole log →
          </RouterLink>
        </div>

        <div class="overflow-hidden rounded-xl border border-slate-800">
          <table class="w-full text-sm">
            <tbody>
              <tr v-if="!events.length && !loading">
                <td class="px-4 py-8 text-center text-slate-500">
                  Nothing has booted through here yet. A machine appears the first time it asks.
                </td>
              </tr>
              <tr
                v-for="event in events"
                :key="event.id"
                class="border-b border-slate-800/70 last:border-0 hover:bg-slate-900/40"
              >
                <td class="whitespace-nowrap px-4 py-2 text-xs text-slate-500" :title="exact(event.at)">
                  {{ ago(event.at) }}
                </td>
                <td class="whitespace-nowrap px-2 py-2">
                  <RouterLink
                    :to="`/devices/${event.mac}`"
                    class="font-mono text-xs text-slate-300 hover:text-sky-300"
                  >
                    {{ event.mac }}
                  </RouterLink>
                </td>
                <td class="px-2 py-2 text-xs" :class="eventTone(event.kind)">{{ event.kind }}</td>
                <td class="px-2 py-2 font-mono text-xs text-slate-400">{{ event.profile || "—" }}</td>
                <td class="max-w-0 truncate px-4 py-2 text-xs text-slate-500" :title="event.detail">
                  {{ event.detail }}
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <section class="space-y-4">
        <div>
          <h2 class="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-500">
            This server
          </h2>
          <dl class="space-y-2 rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3 text-sm">
            <div class="flex justify-between gap-4">
              <dt class="text-slate-500">machines reach</dt>
              <dd class="font-mono text-xs text-slate-300">{{ session.server || "—" }}</dd>
            </div>
            <div class="flex justify-between gap-4">
              <dt class="text-slate-500">http</dt>
              <dd class="truncate font-mono text-xs text-slate-300">{{ session.base || "—" }}</dd>
            </div>
            <div class="flex justify-between gap-4">
              <dt class="text-slate-500">policy</dt>
              <dd class="truncate font-mono text-xs text-slate-300">
                {{ health?.rules?.path || "—" }}
              </dd>
            </div>
            <div class="flex justify-between gap-4">
              <dt class="text-slate-500">loaded</dt>
              <dd class="text-xs text-slate-300" :title="exact(health?.rules?.loaded_at)">
                {{ ago(health?.rules?.loaded_at) }}
              </dd>
            </div>
            <div class="flex justify-between gap-4">
              <dt class="text-slate-500">version</dt>
              <dd class="font-mono text-xs text-slate-300">{{ session.version || "—" }}</dd>
            </div>
          </dl>
        </div>

        <div>
          <h2 class="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-500">
            From a terminal
          </h2>
          <div class="space-y-1.5 rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3 font-mono text-xs text-slate-400">
            <div><span class="text-slate-600">$</span> pxe doctor</div>
            <div><span class="text-slate-600">$</span> pxe test --mac=… --script</div>
            <div><span class="text-slate-600">$</span> pxe log --mac=…</div>
            <div><span class="text-slate-600">$</span> pxe pin --mac=… --profile=… --once</div>
          </div>
          <p class="mt-2 text-xs text-slate-600">
            Everything on these screens is on the JSON API too. Nothing here is browser-only.
          </p>
        </div>
      </section>
    </div>
  </div>
</template>
