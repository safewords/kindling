<script setup>
import { computed, onMounted, ref } from "vue";
import { RouterLink } from "vue-router";
import { api } from "../lib/api.js";
import { ago, eventTone, exact } from "../lib/format.js";
import { useLive } from "../lib/live.js";
import { notify } from "../lib/toast.js";

/** As many rows as the first load asks for; the oldest fall off the end. */
const KEEP = 300;

const events = ref([]);
const loading = ref(true);
const kind = ref("");
const follow = ref(true);
/** Rows that arrived while paused, so unpausing can say how many. */
const held = ref(0);
/** Ids that came over the feed rather than with the page, to highlight. */
const arrived = new Set();

const KINDS = [
  ["", "everything"],
  ["offer", "offers"],
  ["tftp", "TFTP reads"],
  ["http", "HTTP reads"],
  ["script", "scripts served"],
  ["refused", "refusals"],
];

async function load() {
  try {
    const log = await api.events({ limit: KEEP });
    // Rows that arrived over the feed while this request was in flight are
    // newer than anything in the answer, and would otherwise be overwritten
    // by it.
    const newest = log.data[0]?.id ?? 0;
    const crossed = events.value.filter((e) => e.id > newest);
    events.value = [...crossed, ...log.data].slice(0, KEEP);
    held.value = 0;
    arrived.clear();
  } catch (e) {
    notify.error(e);
  } finally {
    loading.value = false;
  }
}

const visible = computed(() =>
  kind.value ? events.value.filter((e) => e.kind === kind.value) : events.value,
);

useLive({
  event: (row) => {
    // Paused means the table holds still while somebody reads it. The rows are
    // not kept, only counted: unpausing loads the log, which is the truth and
    // is already in order.
    if (!follow.value) {
      held.value += 1;
      return;
    }
    // A row already here (the load and the message crossed) is not doubled.
    if (events.value.some((e) => e.id === row.id)) return;
    arrived.add(row.id);
    events.value.unshift(row);
    if (events.value.length > KEEP) events.value.splice(KEEP);
  },
  resync: () => follow.value && load(),
});

function toggleFollow() {
  if (follow.value && held.value) load();
}

onMounted(load);
</script>

<template>
  <div class="space-y-4">
    <div class="flex flex-wrap items-center gap-3">
      <div class="flex flex-wrap gap-1">
        <button
          v-for="[value, label] in KINDS"
          :key="value"
          class="rounded-full border px-2.5 py-1 text-xs transition-colors"
          :class="
            kind === value
              ? 'border-sky-400/50 bg-sky-400/15 text-sky-200'
              : 'border-slate-700 text-slate-400 hover:border-slate-600'
          "
          @click="kind = value"
        >
          {{ label }}
        </button>
      </div>

      <label class="ml-auto flex items-center gap-2 text-xs text-slate-400">
        <span v-if="!follow && held" class="text-sky-300">{{ held }} new</span>
        <input type="checkbox" v-model="follow" class="accent-sky-400" @change="toggleFollow" />
        follow
      </label>
    </div>

    <p class="text-sm text-slate-500">
      One row per thing that happened: an offer, a file fetched, a script served, a refusal. A
      refusal is often the <i>correct</i> outcome — a machine the policy was told to leave alone.
    </p>

    <div class="overflow-x-auto rounded-xl border border-slate-800">
      <table class="w-full text-sm">
        <thead class="sticky-head bg-slate-900/80 text-left text-[11px] uppercase tracking-wider text-slate-500 backdrop-blur">
          <tr>
            <th class="px-4 py-2 font-semibold">When</th>
            <th class="px-3 py-2 font-semibold">Machine</th>
            <th class="px-3 py-2 font-semibold">What</th>
            <th class="px-3 py-2 font-semibold">Profile</th>
            <th class="px-3 py-2 font-semibold">From</th>
            <th class="px-3 py-2 font-semibold">Rule</th>
            <th class="px-4 py-2 font-semibold">Detail</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="loading">
            <td colspan="7" class="px-4 py-10 text-center text-slate-500">Loading…</td>
          </tr>
          <tr v-else-if="!visible.length">
            <td colspan="7" class="px-4 py-10 text-center text-slate-500">Nothing here yet.</td>
          </tr>

          <tr
            v-for="event in visible"
            :key="event.id"
            class="border-t border-slate-800/60 hover:bg-slate-900/40"
            :class="{ arrived: arrived.has(event.id) }"
          >
            <td class="whitespace-nowrap px-4 py-1.5 text-xs text-slate-500" :title="exact(event.at)">
              {{ ago(event.at) }}
            </td>
            <td class="whitespace-nowrap px-3 py-1.5">
              <RouterLink
                v-if="!event.mac.startsWith('ip:')"
                :to="`/devices/${event.mac}`"
                class="font-mono text-xs text-slate-300 hover:text-sky-300"
              >
                {{ event.mac }}
              </RouterLink>
              <!-- TFTP carries no hardware address, so a read from an address
                   this server cannot match to a machine is logged as one. -->
              <span v-else class="font-mono text-xs text-slate-600" title="TFTP has no field for a hardware address">
                {{ event.mac }}
              </span>
            </td>
            <td class="px-3 py-1.5 text-xs" :class="eventTone(event.kind)">{{ event.kind }}</td>
            <td class="whitespace-nowrap px-3 py-1.5 font-mono text-xs text-slate-400">
              {{ event.profile || "—" }}
            </td>
            <td class="px-3 py-1.5 text-xs text-slate-500">{{ event.source || "—" }}</td>
            <td class="whitespace-nowrap px-3 py-1.5 font-mono text-[11px] text-slate-600">
              {{ event.rule || "—" }}
            </td>
            <td class="max-w-0 truncate px-4 py-1.5 text-xs text-slate-500" :title="event.detail">
              {{ event.detail }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
