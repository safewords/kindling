<script setup>
import { computed, onMounted, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import Modal from "../components/Modal.vue";
import { api } from "../lib/api.js";
import { ago, deviceTone, exact } from "../lib/format.js";
import { coalesced, useLive } from "../lib/live.js";
import { notify } from "../lib/toast.js";

const rows = ref([]);
const total = ref(0);
const loading = ref(true);
const search = ref("");
const tag = ref("");
const facets = ref({ tags: [], vendors: [], arches: [] });
const profiles = ref([]);

/** Client-side narrowing on top of the server's search, for the facet chips. */
const vendor = ref("");
const arch = ref("");

const selected = ref(new Set());
const bulk = ref(null); // the open bulk dialog, or null
const bulkProfile = ref("");
const bulkTags = ref("");
const working = ref(false);

let debounce = null;

/** Machines first heard of over the feed, to highlight when they appear. */
const arrived = new Set();

async function load() {
  loading.value = true;
  try {
    const [inventory, facet, profileList] = await Promise.all([
      api.hosts({ search: search.value, tag: tag.value, per_page: 500 }),
      api.facets(),
      api.policy().then((p) => p.profiles).catch(() => []),
    ]);
    rows.value = inventory.data;
    total.value = inventory.total;
    facets.value = facet;
    profiles.value = profileList;
    // A machine that scrolled out of the filter should not stay selected and
    // then be acted on invisibly.
    selected.value = new Set([...selected.value].filter((mac) => inventory.data.some((h) => h.mac === mac)));
  } catch (e) {
    notify.error(e);
  } finally {
    loading.value = false;
  }
}

watch([search, tag], () => {
  clearTimeout(debounce);
  debounce = setTimeout(load, 220);
});

onMounted(load);

/**
 * Load again, but not for every message in a burst.
 *
 * The server did the searching, the tag filter and the ordering, so a machine
 * this page has not got — new, or newly matching — is fetched rather than
 * guessed at: whether it belongs on this page is the server's question.
 */
const refresh = coalesced(load, 750);

/** The facet counts and the profile list, which any change can move. */
const refacet = coalesced(async () => {
  try {
    facets.value = await api.facets();
  } catch {
    // Quiet; the connection indicator already says so if the server is gone.
  }
}, 750);

useLive({
  host: (host) => {
    const index = rows.value.findIndex((h) => h.mac === host.mac);
    if (index === -1) {
      arrived.add(host.mac);
      refresh();
      return;
    }
    // Patched in place, where it is, rather than moved to the top by its new
    // `last_seen`. A row that jumped while somebody was reaching for its
    // checkbox would have them selecting its neighbour.
    rows.value.splice(index, 1, host);
    refacet();
    // Under a filter the change may have taken it out of the result (a tag
    // removed while filtering by that tag), and only the server can say.
    if (search.value || tag.value) refresh();
  },
  "host.forgotten": ({ mac }) => {
    rows.value = rows.value.filter((h) => h.mac !== mac);
    if (selected.value.has(mac)) {
      const next = new Set(selected.value);
      next.delete(mac);
      selected.value = next;
    }
    refresh();
  },
  policy: async () => {
    try {
      profiles.value = (await api.policy()).profiles;
    } catch {
      // The bulk dialog keeps the list it had.
    }
  },
  resync: refresh,
});

const visible = computed(() =>
  rows.value.filter(
    (host) =>
      (!vendor.value || host.vendor === vendor.value) && (!arch.value || host.arch === arch.value),
  ),
);

const allSelected = computed(
  () => visible.value.length > 0 && visible.value.every((h) => selected.value.has(h.mac)),
);

function toggleAll() {
  const next = new Set(selected.value);
  if (allSelected.value) visible.value.forEach((h) => next.delete(h.mac));
  else visible.value.forEach((h) => next.add(h.mac));
  selected.value = next;
}

function toggle(mac) {
  const next = new Set(selected.value);
  next.has(mac) ? next.delete(mac) : next.add(mac);
  selected.value = next;
}

function openBulk(action) {
  bulk.value = action;
  bulkProfile.value = profiles.value[0]?.name || "";
  bulkTags.value = "";
}

async function runBulk() {
  working.value = true;
  try {
    const result = await api.bulk({
      macs: [...selected.value],
      action: bulk.value,
      profile: bulkProfile.value || undefined,
      tags: bulkTags.value
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean),
    });

    const failed = result.results.filter((r) => !r.ok);
    if (failed.length) {
      // Not transactional on purpose — these are independent machines — so
      // the report is per machine rather than one verdict.
      notify.info(
        `${result.changed} of ${result.total} changed.`,
        failed.map((f) => `${f.mac}: ${f.error}`).join("\n"),
      );
    } else {
      notify.ok(`${result.changed} machine${result.changed === 1 ? "" : "s"} changed.`);
    }

    bulk.value = null;
    selected.value = new Set();
    await load();
  } catch (e) {
    notify.error(e);
  } finally {
    working.value = false;
  }
}

const BULK_LABELS = {
  pin: "Pin to a profile",
  unpin: "Remove the pin",
  once: "Boot once",
  "clear-once": "Clear the one-shot",
  tag: "Add tags",
  untag: "Remove tags",
  forget: "Forget these machines",
};
</script>

<template>
  <div class="space-y-4">
    <div class="flex flex-wrap items-center gap-3">
      <input
        v-model="search"
        type="search"
        placeholder="address, hostname, vendor, product or serial"
        class="w-full max-w-md rounded-lg border border-slate-800 bg-slate-900/60 px-3 py-2 text-sm outline-none placeholder:text-slate-600 focus:border-slate-600 sm:w-96"
      />
      <span class="text-xs text-slate-500">
        {{ visible.length }}<span v-if="visible.length !== total"> of {{ total }}</span>
        machine{{ total === 1 ? "" : "s" }}
      </span>
      <button
        class="ml-auto rounded-lg border border-slate-800 px-3 py-2 text-xs text-slate-400 hover:border-slate-700 hover:text-slate-200"
        @click="load"
      >
        Refresh
      </button>
    </div>

    <!-- Facets, counted by the server across the whole inventory rather than
         by the browser across the page it happens to be holding. -->
    <div v-if="facets.tags.length || facets.vendors.length" class="flex flex-wrap gap-4 text-xs">
      <div v-if="facets.tags.length" class="flex flex-wrap items-center gap-1.5">
        <span class="text-slate-600">tag</span>
        <button
          v-for="t in facets.tags"
          :key="t.name"
          class="rounded-full border px-2 py-0.5 transition-colors"
          :class="
            tag === t.name
              ? 'border-sky-400/50 bg-sky-400/15 text-sky-200'
              : 'border-slate-700 text-slate-400 hover:border-slate-600'
          "
          @click="tag = tag === t.name ? '' : t.name"
        >
          {{ t.name }} <span class="opacity-50">{{ t.count }}</span>
        </button>
      </div>

      <div v-if="facets.vendors.length" class="flex flex-wrap items-center gap-1.5">
        <span class="text-slate-600">vendor</span>
        <button
          v-for="v in facets.vendors"
          :key="v.name"
          class="rounded-full border px-2 py-0.5 transition-colors"
          :class="
            vendor === v.name
              ? 'border-sky-400/50 bg-sky-400/15 text-sky-200'
              : 'border-slate-700 text-slate-400 hover:border-slate-600'
          "
          @click="vendor = vendor === v.name ? '' : v.name"
        >
          {{ v.name }} <span class="opacity-50">{{ v.count }}</span>
        </button>
      </div>

      <div v-if="facets.arches.length" class="flex flex-wrap items-center gap-1.5">
        <span class="text-slate-600">arch</span>
        <button
          v-for="a in facets.arches"
          :key="a.name"
          class="rounded-full border px-2 py-0.5 font-mono transition-colors"
          :class="
            arch === a.name
              ? 'border-sky-400/50 bg-sky-400/15 text-sky-200'
              : 'border-slate-700 text-slate-400 hover:border-slate-600'
          "
          @click="arch = arch === a.name ? '' : a.name"
        >
          {{ a.name }} <span class="opacity-50">{{ a.count }}</span>
        </button>
      </div>
    </div>

    <!-- The bulk bar only exists while something is selected, so it never
         sits there inviting a click on nothing. -->
    <div
      v-if="selected.size"
      class="rise sticky top-14 z-20 flex flex-wrap items-center gap-2 rounded-lg border border-sky-500/30 bg-sky-500/10 px-3 py-2 text-sm backdrop-blur"
    >
      <span class="font-medium text-sky-100">{{ selected.size }} selected</span>
      <div class="ml-2 flex flex-wrap gap-1.5">
        <button v-for="(label, action) in BULK_LABELS" :key="action"
          class="rounded border border-sky-400/30 px-2 py-1 text-xs text-sky-100 hover:bg-sky-400/15"
          :class="action === 'forget' ? 'border-rose-400/40 text-rose-200 hover:bg-rose-400/15' : ''"
          @click="openBulk(action)">
          {{ label }}
        </button>
      </div>
      <button class="ml-auto text-xs text-sky-300 hover:text-sky-100" @click="selected = new Set()">
        clear
      </button>
    </div>

    <div class="overflow-x-auto rounded-xl border border-slate-800">
      <table class="w-full text-sm">
        <thead class="sticky-head bg-slate-900/80 text-left text-[11px] uppercase tracking-wider text-slate-500 backdrop-blur">
          <tr>
            <th class="w-8 px-3 py-2">
              <input type="checkbox" :checked="allSelected" class="accent-sky-400" @change="toggleAll" />
            </th>
            <th class="px-3 py-2 font-semibold">Address</th>
            <th class="px-3 py-2 font-semibold">Vendor</th>
            <th class="px-3 py-2 font-semibold">Name</th>
            <th class="px-3 py-2 font-semibold">Arch</th>
            <th class="px-3 py-2 font-semibold">Last boot</th>
            <th class="px-3 py-2 font-semibold">Override</th>
            <th class="px-3 py-2 font-semibold">Tags</th>
            <th class="px-3 py-2 text-right font-semibold">Seen</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="loading && !rows.length">
            <td colspan="9" class="px-4 py-10 text-center text-slate-500">Loading…</td>
          </tr>
          <tr v-else-if="!visible.length">
            <td colspan="9" class="px-4 py-10 text-center text-slate-500">
              No machines match. They are recorded the first time they ask to boot — nothing here
              is declared by hand.
            </td>
          </tr>

          <tr
            v-for="host in visible"
            :key="host.mac"
            class="border-t border-slate-800/70 hover:bg-slate-900/40"
            :class="[selected.has(host.mac) ? 'bg-sky-500/5' : '', { arrived: arrived.has(host.mac) }]"
          >
            <td class="px-3 py-2">
              <input
                type="checkbox"
                class="accent-sky-400"
                :checked="selected.has(host.mac)"
                @change="toggle(host.mac)"
              />
            </td>
            <td class="whitespace-nowrap px-3 py-2">
              <RouterLink :to="`/devices/${host.mac}`" class="font-mono text-xs text-slate-200 hover:text-sky-300">
                {{ host.mac }}
              </RouterLink>
            </td>
            <td class="whitespace-nowrap px-3 py-2">
              <span class="rounded border px-1.5 py-0.5 text-[11px]" :class="deviceTone(host.device_class)">
                {{ host.vendor || "unknown" }}
              </span>
            </td>
            <td class="max-w-[16rem] truncate px-3 py-2 text-slate-300">
              {{ host.hostname || host.product || "—" }}
            </td>
            <td class="whitespace-nowrap px-3 py-2 font-mono text-xs text-slate-500">{{ host.arch }}</td>
            <td class="whitespace-nowrap px-3 py-2 font-mono text-xs text-slate-400">
              {{ host.last_profile || "—" }}
            </td>
            <td class="whitespace-nowrap px-3 py-2 text-xs">
              <span v-if="host.once_profile" class="rounded border border-amber-400/40 bg-amber-400/10 px-1.5 py-0.5 text-amber-200">
                once → {{ host.once_profile }}
              </span>
              <span v-else-if="host.pinned_profile" class="rounded border border-violet-400/40 bg-violet-400/10 px-1.5 py-0.5 text-violet-200">
                pinned → {{ host.pinned_profile }}
              </span>
              <span v-else class="text-slate-600">rules</span>
            </td>
            <td class="px-3 py-2">
              <span
                v-for="t in host.tags"
                :key="t"
                class="mr-1 inline-block rounded-full border border-slate-700 px-1.5 py-0.5 text-[11px] text-slate-400"
              >
                {{ t }}
              </span>
              <span v-if="!host.tags.length" class="text-slate-600">—</span>
            </td>
            <td class="whitespace-nowrap px-3 py-2 text-right text-xs text-slate-500" :title="exact(host.last_seen)">
              {{ ago(host.last_seen) }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <Modal
      v-if="bulk"
      :title="BULK_LABELS[bulk]"
      :subtitle="`${selected.size} machine${selected.size === 1 ? '' : 's'}`"
      @close="bulk = null"
    >
      <div v-if="bulk === 'pin' || bulk === 'once'" class="space-y-2">
        <label class="block text-xs uppercase tracking-wide text-slate-500">Profile</label>
        <select
          v-model="bulkProfile"
          class="w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm outline-none focus:border-slate-500"
        >
          <option v-for="p in profiles" :key="p.name" :value="p.name">
            {{ p.name }} — {{ p.label }}
          </option>
        </select>
        <p class="text-xs text-slate-500">
          <template v-if="bulk === 'once'">
            Spent when each machine is actually served its script, not when it is offered a boot
            file — so a machine that never comes back keeps its one-shot.
          </template>
          <template v-else>
            A pin beats the rules and stays until it is removed.
          </template>
        </p>
      </div>

      <div v-else-if="bulk === 'tag' || bulk === 'untag'" class="space-y-2">
        <label class="block text-xs uppercase tracking-wide text-slate-500">Tags, comma separated</label>
        <input
          v-model="bulkTags"
          placeholder="hold, lab"
          class="w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-sm outline-none focus:border-slate-500"
        />
        <p class="text-xs text-slate-500">
          Tags are what a rule's <code class="font-mono">tag = [...]</code> condition matches — how
          a machine is carved out of a policy without naming its address in the file.
        </p>
      </div>

      <div v-else-if="bulk === 'forget'" class="text-sm text-slate-300">
        <p>
          These machines are removed from the inventory along with their history, pins and tags.
        </p>
        <p class="mt-2 text-amber-200">
          Each one becomes <b>new</b> again, so a rule on
          <code class="font-mono">known = false</code> will fire for it on its next boot.
        </p>
      </div>

      <div v-else class="text-sm text-slate-300">
        This removes the override and lets the rules decide again.
      </div>

      <template #actions>
        <button class="rounded-lg px-3 py-1.5 text-sm text-slate-400 hover:text-slate-200" @click="bulk = null">
          Cancel
        </button>
        <button
          class="rounded-lg px-3 py-1.5 text-sm font-semibold text-slate-950 disabled:opacity-50"
          :class="bulk === 'forget' ? 'bg-rose-400 hover:bg-rose-300' : 'bg-sky-400 hover:bg-sky-300'"
          :disabled="working"
          @click="runBulk"
        >
          {{ working ? "Working…" : BULK_LABELS[bulk] }}
        </button>
      </template>
    </Modal>
  </div>
</template>
