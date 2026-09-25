<script setup>
import { computed, onMounted, ref, watch } from "vue";
import { RouterLink, useRouter } from "vue-router";
import Modal from "../components/Modal.vue";
import { api, session } from "../lib/api.js";
import { ago, deviceTone, eventTone, exact } from "../lib/format.js";
import { coalesced, useLive } from "../lib/live.js";
import { notify } from "../lib/toast.js";

const props = defineProps({ mac: { type: String, required: true } });
const router = useRouter();

const host = ref(null);
const events = ref([]);
const profiles = ref([]);
const decision = ref(null);
const loading = ref(true);
const newTag = ref("");
const confirmForget = ref(false);
const choosing = ref(null); // 'pin' | 'once' | null
const chosen = ref("");

/** `quiet` keeps what is on screen while it loads — for a refresh nobody asked for. */
async function load({ quiet = false } = {}) {
  if (!quiet) loading.value = true;
  try {
    const [detail, policy] = await Promise.all([
      api.host(props.mac),
      api.policy().catch(() => ({ profiles: [] })),
    ]);
    host.value = detail.host;
    events.value = detail.events;
    profiles.value = policy.profiles || [];
    await predict();
  } catch (e) {
    if (!quiet) notify.error(e);
    host.value = null;
  } finally {
    loading.value = false;
  }
}

/**
 * What this machine would boot if it asked right now.
 *
 * The same dry run `pxe:test` performs, against the real inventory row — so
 * the answer accounts for this machine's pins and tags rather than being a
 * generic preview. Nothing is written by it.
 */
async function predict() {
  if (!host.value) return;
  try {
    decision.value = await api.test({
      mac: host.value.mac,
      arch: host.value.arch,
      product: host.value.product,
      manufacturer: host.value.manufacturer,
      serial: host.value.serial,
      hostname: host.value.hostname,
      stage: "ipxe",
    });
  } catch {
    decision.value = null;
  }
}

async function act(fn, message) {
  try {
    await fn();
    notify.ok(message);
    await load();
  } catch (e) {
    notify.error(e);
  }
}

const applyChoice = () =>
  act(
    () => (choosing.value === "pin" ? api.pin(props.mac, chosen.value) : api.once(props.mac, chosen.value)),
    choosing.value === "pin" ? `Pinned to ${chosen.value}.` : `Next boot only: ${chosen.value}.`,
  ).then(() => (choosing.value = null));

const addTag = () => {
  const tag = newTag.value.trim();
  if (!tag) return;
  newTag.value = "";
  return act(() => api.tag(props.mac, [tag]), `Tagged ${tag}.`);
};

const forget = async () => {
  try {
    await api.forget(props.mac);
    notify.ok("Forgotten. Its next boot will look like a first sighting.");
    router.push("/devices");
  } catch (e) {
    notify.error(e);
  }
};

function openChooser(kind) {
  choosing.value = kind;
  chosen.value = host.value?.pinned_profile || profiles.value[0]?.name || "";
}

const FACTS = [
  ["vendor", "Vendor"],
  ["device_class", "Device class"],
  ["arch", "Architecture"],
  ["manufacturer", "Manufacturer"],
  ["product", "Product"],
  ["serial", "Serial"],
  ["asset", "Asset tag"],
  ["uuid", "SMBIOS UUID"],
  ["hostname", "Hostname"],
  ["last_ip", "Last address"],
];

/** The prediction depends on the row and on the policy; either changing re-asks. */
const repredict = coalesced(predict, 500);

/** Loose, because the feed writes the address as the server does. */
const isThis = (mac) => mac?.toLowerCase() === props.mac.toLowerCase();

useLive({
  host: (row) => {
    if (!isThis(row.mac)) return;
    host.value = row;
    repredict();
  },
  event: (row) => {
    if (!isThis(row.mac) || events.value.some((e) => e.id === row.id)) return;
    events.value = [row, ...events.value].slice(0, 50);
  },
  "host.forgotten": ({ mac }) => {
    if (!isThis(mac)) return;
    // This tab's own "Forget" navigates away by itself; this is for somebody
    // else's, so the page does not go on showing a machine that is gone.
    if (Date.now() - session.lastWrite < 3000) return;
    notify.info(`${mac} was forgotten elsewhere. Its next boot will look like a first sighting.`);
    host.value = null;
  },
  policy: async () => {
    try {
      profiles.value = (await api.policy()).profiles || [];
    } catch {
      // The chooser keeps the list it had.
    }
    repredict();
  },
  resync: () => load({ quiet: true }),
});

const known = computed(() => (host.value?.boot_count ?? 0) > 0);

watch(() => props.mac, load);
onMounted(load);
</script>

<template>
  <div v-if="loading" class="py-16 text-center text-slate-500">Loading…</div>

  <div v-else-if="!host" class="py-16 text-center">
    <p class="text-slate-400">No machine with that address has been seen here.</p>
    <RouterLink to="/devices" class="mt-2 inline-block text-sm text-sky-400 hover:underline">
      ← every machine
    </RouterLink>
  </div>

  <div v-else class="space-y-6">
    <div class="flex flex-wrap items-start gap-4">
      <div class="flex-1">
        <RouterLink to="/devices" class="text-xs text-slate-500 hover:text-slate-300">
          ← machines
        </RouterLink>
        <h1 class="mt-1 font-mono text-xl text-slate-100">{{ host.mac }}</h1>
        <p class="mt-1 flex flex-wrap items-center gap-2 text-sm text-slate-400">
          <span class="rounded border px-1.5 py-0.5 text-[11px]" :class="deviceTone(host.device_class)">
            {{ host.device_class }}
          </span>
          <span>{{ host.vendor || "unknown vendor" }}</span>
          <span v-if="host.product" class="text-slate-500">· {{ host.product }}</span>
          <span class="text-slate-600">·</span>
          <span :class="known ? 'text-slate-500' : 'text-emerald-400'">
            {{ known ? `${host.boot_count} boot${host.boot_count === 1 ? "" : "s"} here` : "never booted here" }}
          </span>
        </p>
      </div>

      <div class="flex flex-wrap gap-2">
        <button
          class="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-300 hover:border-slate-600"
          @click="openChooser('once')"
        >
          Boot once…
        </button>
        <button
          class="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-300 hover:border-slate-600"
          @click="openChooser('pin')"
        >
          Pin…
        </button>
        <button
          class="rounded-lg border border-rose-500/40 px-3 py-1.5 text-sm text-rose-300 hover:bg-rose-500/10"
          @click="confirmForget = true"
        >
          Forget
        </button>
      </div>
    </div>

    <!-- The override banner. A pin is invisible in a table of forty machines
         and is exactly the thing somebody forgets they set. -->
    <div
      v-if="host.once_profile || host.pinned_profile"
      class="flex flex-wrap items-center gap-3 rounded-lg border px-4 py-3 text-sm"
      :class="
        host.once_profile
          ? 'border-amber-400/40 bg-amber-400/10 text-amber-100'
          : 'border-violet-400/40 bg-violet-400/10 text-violet-100'
      "
    >
      <template v-if="host.once_profile">
        <span>
          <b>Next boot only:</b> <code class="font-mono">{{ host.once_profile }}</code>.
          Spent when the script is served, not when a file is offered.
        </span>
        <button class="ml-auto text-xs underline" @click="act(() => api.clearOnce(mac), 'One-shot cleared.')">
          clear
        </button>
      </template>
      <template v-else>
        <span>
          <b>Pinned:</b> <code class="font-mono">{{ host.pinned_profile }}</code>, whatever the rules say.
        </span>
        <button class="ml-auto text-xs underline" @click="act(() => api.unpin(mac), 'Pin removed.')">
          remove
        </button>
      </template>
    </div>

    <div class="grid gap-6 lg:grid-cols-3">
      <section class="space-y-4">
        <div>
          <h2 class="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-500">
            What this machine told us
          </h2>
          <dl class="divide-y divide-slate-800 rounded-xl border border-slate-800 bg-slate-900/40 text-sm">
            <div v-for="[key, label] in FACTS" :key="key" class="flex justify-between gap-4 px-4 py-2">
              <dt class="text-slate-500">{{ label }}</dt>
              <dd class="truncate text-right font-mono text-xs text-slate-300" :title="host[key] || ''">
                {{ host[key] || "—" }}
              </dd>
            </div>
            <div class="flex justify-between gap-4 px-4 py-2">
              <dt class="text-slate-500">First seen</dt>
              <dd class="text-xs text-slate-300" :title="exact(host.first_seen)">{{ ago(host.first_seen) }}</dd>
            </div>
            <div class="flex justify-between gap-4 px-4 py-2">
              <dt class="text-slate-500">Last seen</dt>
              <dd class="text-xs text-slate-300" :title="exact(host.last_seen)">{{ ago(host.last_seen) }}</dd>
            </div>
          </dl>
        </div>

        <div>
          <h2 class="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-500">Tags</h2>
          <div class="rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3">
            <div class="flex flex-wrap gap-1.5">
              <span
                v-for="t in host.tags"
                :key="t"
                class="group inline-flex items-center gap-1 rounded-full border border-slate-700 px-2 py-0.5 text-xs text-slate-300"
              >
                {{ t }}
                <button
                  class="text-slate-600 hover:text-rose-400"
                  @click="act(() => api.tag(mac, [], [t]), `Removed ${t}.`)"
                >
                  ✕
                </button>
              </span>
              <span v-if="!host.tags.length" class="text-xs text-slate-600">none</span>
            </div>
            <input
              v-model="newTag"
              placeholder="add a tag and press enter"
              class="mt-3 w-full rounded-lg border border-slate-800 bg-slate-950 px-3 py-1.5 font-mono text-xs outline-none placeholder:text-slate-600 focus:border-slate-600"
              @keyup.enter="addTag"
            />
          </div>
        </div>
      </section>

      <section class="lg:col-span-2 space-y-4">
        <!-- The question somebody opens this page to ask. -->
        <div>
          <h2 class="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-500">
            What it would boot right now
          </h2>
          <div class="rounded-xl border border-slate-800 bg-slate-900/40 p-4">
            <div v-if="!decision" class="text-sm text-slate-500">Could not work it out.</div>
            <template v-else>
              <div class="flex flex-wrap items-baseline gap-3">
                <code class="text-lg text-sky-300">{{ decision.decision.profile || "nothing" }}</code>
                <span class="rounded-full border border-slate-700 px-2 py-0.5 text-[11px] text-slate-400">
                  from the {{ decision.decision.source }}
                </span>
              </div>
              <p class="mt-1 text-sm text-slate-400">{{ decision.decision.reason }}</p>

              <details class="mt-3">
                <summary class="cursor-pointer text-xs text-slate-500 hover:text-slate-300">
                  every rule, and why it did or did not fire
                </summary>
                <table class="mt-2 w-full text-xs">
                  <tr
                    v-for="entry in decision.trace"
                    :key="entry.rule"
                    class="border-t border-slate-800/60"
                  >
                    <td class="py-1 pr-4 font-mono text-slate-300">{{ entry.rule }}</td>
                    <td class="py-1 text-slate-500">
                      <span v-if="entry.outcome === 'matched'" class="text-emerald-400">
                        matched<template v-if="entry.profile"> → {{ entry.profile }}</template>
                        <template v-else> (tags only)</template>
                        <template v-if="entry.superseded">, but an earlier rule had chosen</template>
                        <template v-if="entry.stopped">, stops here</template>
                      </span>
                      <span v-else-if="entry.outcome === 'no-match'">no match ({{ entry.field }})</span>
                      <span v-else-if="entry.outcome === 'excluded'" class="text-amber-400">
                        matched, excluded by <code>unless</code>
                      </span>
                      <span v-else class="text-slate-600">disabled</span>
                    </td>
                  </tr>
                </table>
              </details>

              <details v-if="decision.script" class="mt-2">
                <summary class="cursor-pointer text-xs text-slate-500 hover:text-slate-300">
                  the script it would be served
                </summary>
                <pre class="mt-2 max-h-72 overflow-auto rounded-lg border border-slate-800 bg-slate-950 p-3 font-mono text-[11px] text-slate-300">{{ decision.script }}</pre>
              </details>
            </template>
          </div>
        </div>

        <div>
          <h2 class="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-500">
            What has happened to it
          </h2>
          <div class="overflow-hidden rounded-xl border border-slate-800">
            <table class="w-full text-sm">
              <tbody>
                <tr v-if="!events.length">
                  <td class="px-4 py-8 text-center text-slate-500">Nothing recorded yet.</td>
                </tr>
                <tr
                  v-for="event in events"
                  :key="event.id"
                  class="border-b border-slate-800/60 last:border-0"
                >
                  <td class="whitespace-nowrap px-4 py-2 text-xs text-slate-500" :title="exact(event.at)">
                    {{ ago(event.at) }}
                  </td>
                  <td class="px-2 py-2 text-xs" :class="eventTone(event.kind)">{{ event.kind }}</td>
                  <td class="px-2 py-2 font-mono text-xs text-slate-400">{{ event.profile || "—" }}</td>
                  <td class="px-2 py-2 font-mono text-[11px] text-slate-600">{{ event.rule || "" }}</td>
                  <td class="max-w-0 truncate px-4 py-2 text-xs text-slate-500" :title="event.detail">
                    {{ event.detail }}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </section>
    </div>

    <Modal
      v-if="choosing"
      :title="choosing === 'pin' ? 'Pin this machine' : 'Boot this once'"
      :subtitle="host.mac"
      @close="choosing = null"
    >
      <label class="block text-xs uppercase tracking-wide text-slate-500">Profile</label>
      <select
        v-model="chosen"
        class="mt-2 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm outline-none focus:border-slate-500"
      >
        <option v-for="p in profiles" :key="p.name" :value="p.name">{{ p.name }} — {{ p.label }}</option>
      </select>
      <p class="mt-2 text-xs text-slate-500">
        <template v-if="choosing === 'once'">
          A one-shot beats a pin and is spent when the machine is actually served its script.
        </template>
        <template v-else>
          A pin beats the rules and stays until somebody removes it.
        </template>
      </p>

      <template #actions>
        <button class="rounded-lg px-3 py-1.5 text-sm text-slate-400 hover:text-slate-200" @click="choosing = null">
          Cancel
        </button>
        <button
          class="rounded-lg bg-sky-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-sky-300"
          @click="applyChoice"
        >
          {{ choosing === "pin" ? "Pin it" : "Set it" }}
        </button>
      </template>
    </Modal>

    <Modal v-if="confirmForget" title="Forget this machine?" :subtitle="host.mac" @close="confirmForget = false">
      <p class="text-sm text-slate-300">
        Its row, history, pins and tags are removed.
      </p>
      <p class="mt-2 text-sm text-amber-200">
        It becomes <b>new</b> again, so a rule on <code class="font-mono">known = false</code> will
        fire for it the next time it boots.
      </p>

      <template #actions>
        <button class="rounded-lg px-3 py-1.5 text-sm text-slate-400 hover:text-slate-200" @click="confirmForget = false">
          Cancel
        </button>
        <button class="rounded-lg bg-rose-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-rose-300" @click="forget">
          Forget it
        </button>
      </template>
    </Modal>
  </div>
</template>
