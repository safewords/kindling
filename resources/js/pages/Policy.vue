<script setup>
import { computed, nextTick, onMounted, ref } from "vue";
import Modal from "../components/Modal.vue";
import ProfileWizard from "../components/ProfileWizard.vue";
import RuleWizard from "../components/RuleWizard.vue";
import { api, session } from "../lib/api.js";
import { ago, exact } from "../lib/format.js";
import { useLive } from "../lib/live.js";
import { loadSchema, schema } from "../lib/schema.js";
import { notify } from "../lib/toast.js";

const tab = ref("rules");
const policy = ref(null);
const loading = ref(true);
const saving = ref(false);

/** The running policy changed while this screen held unsaved work. */
const stale = ref(false);

// --- editors ---
const editingRule = ref(undefined); // undefined = closed, null = new
const ruleSeed = ref(null);
const editingProfile = ref(undefined);
const confirmDelete = ref(null);
const wizard = ref(null);
/** A profile being made from inside the rule wizard. */
const profileForWizard = ref(false);

async function load({ quiet = false } = {}) {
  if (!quiet) loading.value = true;
  try {
    const [loaded] = await Promise.all([api.policy(), loadSchema()]);
    policy.value = loaded;
    session.policyRevision = loaded.revision;
    stale.value = false;
    if (tab.value === "history") loadHistory();
  } catch (e) {
    notify.error(e);
  } finally {
    loading.value = false;
  }
}

onMounted(load);

const busy = computed(() => saving.value || editingRule.value !== undefined || editingProfile.value !== undefined || importing.value);

useLive({
  policy: () => (busy.value ? (stale.value = !saving.value) : load({ quiet: true })),
  resync: () => !busy.value && load({ quiet: true }),
});

async function act(fn, message) {
  saving.value = true;
  try {
    const result = await fn();
    notify.ok(result?.changed === false ? "Nothing changed." : message);
    await load({ quiet: true });
    return true;
  } catch (e) {
    notify.error(e);
    return false;
  } finally {
    saving.value = false;
  }
}

// --- rules ---
function newRule() {
  ruleSeed.value = null;
  editingRule.value = null;
}
function duplicate(rule) {
  ruleSeed.value = rule;
  editingRule.value = null;
}
async function saveRule(payload) {
  const original = editingRule.value?.name;
  const ok = await act(
    () => (original ? api.putRule(original, payload) : api.addRule(payload)),
    original ? `Rule ${payload.name} saved.` : `Rule ${payload.name} added.`,
  );
  if (ok) editingRule.value = undefined;
}
const toggleRule = (rule) =>
  act(() => api.setRuleEnabled(rule.name, !rule.enabled), `${rule.name} ${rule.enabled ? "disabled" : "enabled"}.`);

function move(index, delta) {
  const order = policy.value.rules.map((r) => r.name);
  const [name] = order.splice(index, 1);
  order.splice(index + delta, 0, name);
  act(() => api.reorderRules(order), "Rules reordered.");
}

const filter = ref("");
const shownRules = computed(() => {
  const term = filter.value.trim().toLowerCase();
  const all = policy.value?.rules || [];
  if (!term) return all.map((rule, index) => ({ rule, index }));
  return all
    .map((rule, index) => ({ rule, index }))
    .filter(({ rule }) =>
      [rule.name, rule.description, rule.profile, rule.described?.when, ...(rule.tag || [])]
        .filter(Boolean)
        .some((text) => text.toLowerCase().includes(term)),
    );
});

// --- profiles ---
async function saveProfile({ name, body }) {
  const ok = await act(() => api.putProfile(name, body), `Profile ${body.rename_to || name} saved.`);
  if (!ok) return;
  editingProfile.value = undefined;
  if (profileForWizard.value) {
    profileForWizard.value = false;
    await nextTick();
    wizard.value?.useProfile(body.rename_to || name);
  }
}
function profileFromWizard() {
  profileForWizard.value = true;
  editingProfile.value = null;
}

// --- settings and loaders ---
const settingsDraft = ref({ default_profile: "", timezone_offset_minutes: 0 });
function openSettings() {
  settingsDraft.value = {
    default_profile: policy.value.settings.default_profile || "",
    timezone_offset_minutes: policy.value.settings.timezone_offset_minutes || 0,
  };
}
const saveSettings = () =>
  act(
    () =>
      api.putSettings({
        default_profile: settingsDraft.value.default_profile || null,
        timezone_offset_minutes: Number(settingsDraft.value.timezone_offset_minutes) || 0,
      }),
    "Settings saved.",
  );

const loaderDraft = ref({ arch: "", file: "" });
const loaderRows = computed(() => Object.entries(policy.value?.bootloaders || {}));
const loaderEdits = ref({});
const saveLoader = (arch, file) => act(() => api.setBootloader(arch, file || null), `Loader for ${arch} saved.`);
async function addLoader() {
  const { arch, file } = loaderDraft.value;
  if (!arch.trim() || !file.trim()) return notify.bad("Name an architecture and a file.");
  if (await saveLoader(arch.trim(), file.trim())) loaderDraft.value = { arch: "", file: "" };
}

// --- history ---
const history = ref(null);
const viewing = ref(null);
async function loadHistory() {
  try {
    history.value = await api.revisions();
  } catch (e) {
    notify.error(e);
  }
}
async function view(revision) {
  try {
    viewing.value = await api.revision(revision.id);
  } catch (e) {
    notify.error(e);
  }
}
const restore = (revision) =>
  act(() => api.restoreRevision(revision.id), `Revision ${revision.id} restored.`).then((ok) => ok && (confirmDelete.value = null));

// --- import ---
const importText = ref("");
const importCheck = ref(null);
const importing = ref(false);
async function readFile(event) {
  const file = event.target.files?.[0];
  if (!file) return;
  importText.value = await file.text();
  checkImport();
}
async function checkImport() {
  if (!importText.value.trim()) return (importCheck.value = null);
  importing.value = true;
  try {
    const [validated, impact] = await Promise.all([
      api.validatePolicy({ text: importText.value }),
      api.previewPolicy({ text: importText.value }),
    ]);
    importCheck.value = { ...validated, impact: impact.ok ? impact : null };
  } catch (e) {
    importCheck.value = { ok: false, problems: [e.message] };
  } finally {
    importing.value = false;
  }
}
async function doImport() {
  const ok = await act(() => api.replacePolicy({ text: importText.value }), "Policy imported.");
  if (ok) {
    importText.value = "";
    importCheck.value = null;
  }
}

// --- the tester ---
const subject = ref({ mac: "18:66:da:11:22:33", arch: "x64-uefi", stage: "ipxe", product: "", hostname: "", client_ip: "" });
const result = ref(null);
const testing = ref(false);
async function runTest() {
  testing.value = true;
  try {
    const clean = Object.fromEntries(Object.entries(subject.value).filter(([, v]) => v !== ""));
    result.value = await api.test(clean);
  } catch (e) {
    notify.error(e);
  } finally {
    testing.value = false;
  }
}

function switchTab(key) {
  tab.value = key;
  if (key === "history") loadHistory();
  if (key === "settings") openSettings();
}

const TABS = [
  ["rules", "Rules"],
  ["profiles", "Profiles"],
  ["loaders", "Boot loaders"],
  ["settings", "Settings"],
  ["test", "Test a machine"],
  ["history", "History"],
  ["transfer", "Import / export"],
];

const does = (rule) => {
  const parts = [];
  if (rule.profile) parts.push(["boots", rule.profile, "text-sky-300"]);
  if (rule.tag?.length) parts.push(["tags", `+${rule.tag.join(" +")}`, "text-emerald-300"]);
  if (rule.remove_tags?.length) parts.push(["untags", `−${rule.remove_tags.join(" −")}`, "text-rose-300"]);
  for (const [k, v] of Object.entries(rule.set || {})) parts.push(["sets", `${k}=${v}`, "text-violet-300"]);
  return parts;
};
</script>

<template>
  <div v-if="loading" class="py-16 text-center text-slate-500">Loading…</div>

  <div v-else-if="policy" class="space-y-5">
    <div v-if="policy.last_error" class="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3 text-sm">
      <div class="font-semibold text-rose-200">The stored policy does not load, so what is running is older than it.</div>
      <pre class="mt-2 whitespace-pre-wrap font-mono text-xs text-rose-200/80">{{ policy.last_error.error }}</pre>
    </div>

    <div
      v-if="stale"
      class="flex flex-wrap items-center gap-3 rounded-lg border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm text-amber-100"
    >
      <div class="flex-1">
        Somebody else changed the policy while you were editing. Saving now will be refused until you load their change.
      </div>
      <button class="rounded-md border border-amber-400/50 px-3 py-1.5 text-xs hover:bg-amber-400/10" @click="load({ quiet: true })">
        Load their change
      </button>
    </div>

    <div class="flex flex-wrap items-center gap-4">
      <div class="flex flex-wrap gap-1">
        <button
          v-for="[key, label] in TABS"
          :key="key"
          class="rounded-lg px-3 py-1.5 text-sm transition-colors"
          :class="tab === key ? 'bg-slate-800 text-slate-100' : 'text-slate-400 hover:text-slate-200'"
          @click="switchTab(key)"
        >
          {{ label }}
        </button>
      </div>
      <div class="ml-auto flex items-center gap-3 text-xs text-slate-500">
        <span v-if="policy.revision">revision {{ policy.revision }}</span>
        <span v-if="policy.last_change" :title="exact(policy.last_change.created_at)">
          {{ policy.last_change.summary }} · {{ ago(policy.last_change.created_at) }}
        </span>
      </div>
    </div>

    <!-- Rules -->
    <section v-if="tab === 'rules'" class="space-y-3">
      <div class="flex flex-wrap items-center gap-3">
        <p class="text-sm text-slate-500">
          In evaluation order. The first rule to choose a profile wins; rules that only tag or set variables carry on.
        </p>
        <input
          v-model="filter"
          placeholder="filter…"
          class="ml-auto w-48 rounded-lg border border-slate-700 bg-slate-950 px-3 py-1.5 text-sm outline-none focus:border-slate-500"
        />
        <button class="rounded-lg bg-sky-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-sky-300" @click="newRule">
          New rule
        </button>
      </div>

      <div class="space-y-2">
        <div
          v-for="{ rule, index } in shownRules"
          :key="rule.name"
          class="rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3"
          :class="rule.enabled ? '' : 'opacity-50'"
        >
          <div class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
            <span class="w-10 shrink-0 text-right font-mono text-xs text-slate-600">{{ rule.priority }}</span>
            <button class="font-mono text-sm text-slate-100 hover:text-sky-300" @click="editingRule = rule">{{ rule.name }}</button>
            <span v-for="[verb, what, colour] in does(rule)" :key="verb + what" class="text-xs text-slate-500">
              {{ verb }} <code :class="colour">{{ what }}</code>
            </span>
            <span v-if="!rule.stops" class="text-[11px] text-slate-600">· carries on</span>

            <div class="ml-auto flex items-center gap-2">
              <button
                class="text-xs text-slate-600 hover:text-slate-200 disabled:opacity-30"
                :disabled="filter || index === 0"
                title="Run earlier"
                @click="move(index, -1)"
              >
                ▲
              </button>
              <button
                class="text-xs text-slate-600 hover:text-slate-200 disabled:opacity-30"
                :disabled="filter || index === policy.rules.length - 1"
                title="Run later"
                @click="move(index, 1)"
              >
                ▼
              </button>
              <button
                class="rounded border px-2 py-0.5 text-[11px]"
                :class="rule.enabled ? 'border-emerald-400/40 text-emerald-300 hover:bg-emerald-400/10' : 'border-slate-600 text-slate-400'"
                @click="toggleRule(rule)"
              >
                {{ rule.enabled ? "on" : "off" }}
              </button>
              <button class="text-xs text-slate-500 hover:text-slate-200" @click="editingRule = rule">edit</button>
              <button class="text-xs text-slate-500 hover:text-slate-200" @click="duplicate(rule)">duplicate</button>
              <button class="text-xs text-slate-600 hover:text-rose-400" @click="confirmDelete = { kind: 'rule', name: rule.name }">
                delete
              </button>
            </div>
          </div>
          <p v-if="rule.description" class="mt-1 pl-13 text-xs text-slate-500">{{ rule.description }}</p>
          <p class="mt-1 pl-13 text-xs text-slate-400">
            <span class="text-slate-600">when</span> {{ rule.described?.when }}
            <template v-if="rule.described?.unless">
              <span class="text-amber-400/70"> · unless</span> {{ rule.described.unless }}
            </template>
          </p>
        </div>

        <p v-if="!policy.rules.length" class="rounded-xl border border-slate-800 px-4 py-8 text-center text-sm text-slate-500">
          No rules yet, so every machine gets the default profile. Start with <b>New rule</b>.
        </p>
      </div>
    </section>

    <!-- Profiles -->
    <section v-else-if="tab === 'profiles'" class="space-y-3">
      <div class="flex items-center gap-3">
        <p class="text-sm text-slate-500">
          What a machine can be sent to boot. When no rule chooses, it gets
          <code class="font-mono text-sky-300">{{ policy.settings.default_profile || "nothing" }}</code>.
        </p>
        <button
          class="ml-auto rounded-lg bg-sky-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-sky-300"
          @click="editingProfile = null"
        >
          New profile
        </button>
      </div>

      <div class="grid gap-2 md:grid-cols-2">
        <div v-for="p in policy.profiles" :key="p.name" class="rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3">
          <div class="flex items-baseline gap-2">
            <button class="font-mono text-sm text-slate-100 hover:text-sky-300" @click="editingProfile = p">{{ p.name }}</button>
            <span class="rounded-full border border-slate-700 px-1.5 py-0.5 text-[11px] text-slate-400">{{ p.kind }}</span>
            <span v-if="p.is_default" class="rounded-full border border-sky-500/40 px-1.5 py-0.5 text-[11px] text-sky-300">default</span>
            <div class="ml-auto flex gap-2">
              <button class="text-xs text-slate-500 hover:text-slate-200" @click="editingProfile = p">edit</button>
              <button class="text-xs text-slate-600 hover:text-rose-400" @click="confirmDelete = { kind: 'profile', name: p.name, profile: p }">
                delete
              </button>
            </div>
          </div>
          <p class="mt-0.5 text-sm text-slate-400">{{ p.label }}</p>
          <p v-if="p.body.description" class="mt-1 text-xs text-slate-600">{{ p.body.description }}</p>
          <code v-if="p.body.kernel" class="mt-1.5 block truncate font-mono text-[11px] text-slate-600">{{ p.body.kernel }}</code>
          <p v-if="p.used_by.length || p.in_menus.length" class="mt-1.5 text-[11px] text-slate-600">
            used by
            <span v-for="r in p.used_by" :key="r" class="mr-1 font-mono text-slate-500">{{ r }}</span>
            <span v-for="m in p.in_menus" :key="`m-${m}`" class="mr-1 font-mono text-slate-500">menu:{{ m }}</span>
          </p>
        </div>
      </div>
    </section>

    <!-- Boot loaders -->
    <section v-else-if="tab === 'loaders'" class="space-y-3">
      <p class="text-sm text-slate-500">
        The binary the firmware is handed, by architecture. The wrong one loads, fails and falls through to the disk
        without a word — which is why <code class="font-mono">pxe:doctor</code> checks these exist.
      </p>
      <table class="w-full overflow-hidden rounded-xl border border-slate-800 text-sm">
        <thead class="bg-slate-900/60 text-left text-[11px] uppercase tracking-wider text-slate-500">
          <tr>
            <th class="px-4 py-2 font-semibold">Architecture</th>
            <th class="px-4 py-2 font-semibold">File, inside the boot root (or a URL)</th>
            <th class="w-40"></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="[arch, file] in loaderRows" :key="arch" class="border-t border-slate-800/70">
            <td class="px-4 py-2 font-mono text-xs text-slate-300">{{ arch }}</td>
            <td class="px-4 py-1.5">
              <input
                :value="loaderEdits[arch] ?? file"
                class="w-full rounded border border-slate-800 bg-slate-950 px-2 py-1 font-mono text-xs outline-none focus:border-slate-600"
                @input="loaderEdits[arch] = $event.target.value"
              />
            </td>
            <td class="px-4 py-1.5 text-right">
              <button
                v-if="loaderEdits[arch] !== undefined && loaderEdits[arch] !== file"
                class="mr-3 text-xs text-sky-300"
                @click="saveLoader(arch, loaderEdits[arch]).then(() => delete loaderEdits[arch])"
              >
                save
              </button>
              <button class="text-xs text-slate-600 hover:text-rose-400" @click="saveLoader(arch, null)">remove</button>
            </td>
          </tr>
          <tr class="border-t border-slate-800/70 bg-slate-900/30">
            <td class="px-4 py-1.5">
              <input
                v-model="loaderDraft.arch"
                list="architectures"
                placeholder="arm64-uefi"
                class="w-full rounded border border-slate-800 bg-slate-950 px-2 py-1 font-mono text-xs outline-none"
              />
              <datalist id="architectures">
                <option value="default" />
                <option v-for="a in schema.architectures" :key="a" :value="a" />
              </datalist>
            </td>
            <td class="px-4 py-1.5">
              <input v-model="loaderDraft.file" placeholder="ipxe-arm64.efi" class="w-full rounded border border-slate-800 bg-slate-950 px-2 py-1 font-mono text-xs outline-none" />
            </td>
            <td class="px-4 py-1.5 text-right">
              <button class="text-xs text-sky-300" @click="addLoader">+ add</button>
            </td>
          </tr>
        </tbody>
      </table>
      <p class="text-xs text-slate-600">
        With none declared the built-in names are used: <code>undionly.kpxe</code> for BIOS, <code>ipxe.efi</code> for x64 UEFI.
      </p>
    </section>

    <!-- Settings -->
    <section v-else-if="tab === 'settings'" class="max-w-xl space-y-4">
      <div>
        <label class="block text-xs uppercase tracking-wide text-slate-500">Default profile</label>
        <select v-model="settingsDraft.default_profile" class="mt-1 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm outline-none">
          <option value="">— none: a machine no rule chose is not answered —</option>
          <option v-for="p in policy.profiles" :key="p.name" :value="p.name">{{ p.name }} — {{ p.label }}</option>
        </select>
        <p class="mt-1 text-xs text-slate-600">What a machine boots when no rule chose anything.</p>
      </div>
      <div>
        <label class="block text-xs uppercase tracking-wide text-slate-500">Timezone offset, minutes from UTC</label>
        <input v-model.number="settingsDraft.timezone_offset_minutes" type="number" class="mt-1 w-40 rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-sm outline-none" />
        <p class="mt-1 text-xs text-slate-600">
          Time-of-day and weekday conditions are evaluated in UTC shifted by this — e.g. <code>-420</code> for US Pacific
          daylight time. Stated here rather than taken from the server, whose timezone is whatever its container inherited.
        </p>
      </div>
      <button class="rounded-lg bg-sky-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-sky-300" :disabled="saving" @click="saveSettings">
        Save settings
      </button>
    </section>

    <!-- The tester -->
    <section v-else-if="tab === 'test'" class="space-y-4">
      <p class="text-sm text-slate-500">
        Describe a machine and see what the policy would send it, and why. Nothing is written. A machine the inventory
        already knows is tested with its tags and boot count.
      </p>
      <div class="grid gap-3 sm:grid-cols-3">
        <label v-for="[key, text, ph] in [['mac', 'Address', ''], ['arch', 'Architecture', 'x64-uefi'], ['product', 'Product', 'OptiPlex 7090'], ['hostname', 'Hostname', 'lab-01'], ['client_ip', 'IP address', '10.20.1.5']]" :key="key" class="block">
          <span class="block text-xs uppercase tracking-wide text-slate-500">{{ text }}</span>
          <input v-model="subject[key]" :placeholder="ph" class="mt-1 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-sm outline-none focus:border-slate-500" />
        </label>
        <label class="block">
          <span class="block text-xs uppercase tracking-wide text-slate-500">Stage</span>
          <select v-model="subject.stage" class="mt-1 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm outline-none">
            <option value="firmware">firmware (DHCP)</option>
            <option value="ipxe">iPXE (the script)</option>
          </select>
        </label>
      </div>
      <button class="rounded-lg bg-sky-400 px-4 py-2 text-sm font-semibold text-slate-950 hover:bg-sky-300" :disabled="testing" @click="runTest">
        {{ testing ? "Asking…" : "What would it boot?" }}
      </button>

      <div v-if="result" class="space-y-3 rounded-xl border border-slate-800 bg-slate-900/40 p-4">
        <div class="flex flex-wrap items-baseline gap-3">
          <code class="text-lg text-sky-300">{{ result.decision.profile || "nothing" }}</code>
          <span class="rounded-full border border-slate-700 px-2 py-0.5 text-[11px] text-slate-400">from the {{ result.decision.source }}</span>
          <span v-if="result.firmware" class="font-mono text-xs text-slate-500">firmware would be handed {{ result.firmware.file }}</span>
        </div>
        <p class="text-sm text-slate-400">{{ result.decision.reason }}</p>
        <p v-if="result.decision.tags?.length || result.decision.removed_tags?.length || Object.keys(result.decision.vars || {}).length" class="text-xs text-slate-500">
          <span v-if="result.decision.tags?.length">adds <code class="text-emerald-300">{{ result.decision.tags.join(", ") }}</code> · </span>
          <span v-if="result.decision.removed_tags?.length">removes <code class="text-rose-300">{{ result.decision.removed_tags.join(", ") }}</code> · </span>
          <span v-for="(v, k) in result.decision.vars" :key="k">sets <code class="text-violet-300">{{ k }}={{ v }}</code> </span>
        </p>
        <table class="w-full text-xs">
          <tr v-for="entry in result.trace" :key="entry.rule" class="border-t border-slate-800/60">
            <td class="w-64 py-1 pr-4 font-mono text-slate-300">{{ entry.rule }}</td>
            <td class="py-1 text-slate-500">
              <span v-if="entry.outcome === 'matched'" class="text-emerald-400">
                matched<template v-if="entry.profile"> → {{ entry.profile }}</template>
                <template v-if="entry.superseded">, but an earlier rule had chosen</template>
                <template v-if="entry.stopped">, stops here</template>
              </span>
              <span v-else-if="entry.outcome === 'no-match'">no match — {{ entry.detail || entry.field }}</span>
              <span v-else-if="entry.outcome === 'excluded'" class="text-amber-400">matched, excluded by its exception</span>
              <span v-else class="text-slate-600">disabled</span>
            </td>
          </tr>
        </table>
        <details>
          <summary class="cursor-pointer text-xs text-slate-500 hover:text-slate-300">the script</summary>
          <pre class="mt-2 max-h-80 overflow-auto rounded-lg border border-slate-800 bg-slate-950 p-3 font-mono text-[11px] text-slate-300">{{ result.script }}</pre>
        </details>
      </div>
    </section>

    <!-- History -->
    <section v-else-if="tab === 'history'" class="space-y-3">
      <p class="text-sm text-slate-500">
        Every change to the policy, newest first, each kept whole. Restoring one is itself a change, so it can be undone too.
      </p>
      <div v-if="!history" class="text-sm text-slate-500">Loading…</div>
      <table v-else class="w-full overflow-hidden rounded-xl border border-slate-800 text-sm">
        <tr v-for="r in history.data" :key="r.id" class="border-t border-slate-800/70 first:border-t-0">
          <td class="w-16 px-4 py-2 font-mono text-xs text-slate-500">
            {{ r.id }}<span v-if="r.id === history.current" class="text-sky-300"> ●</span>
          </td>
          <td class="px-2 py-2 text-slate-300">{{ r.summary }}</td>
          <td class="px-2 py-2 text-xs text-slate-500">{{ r.actor }}</td>
          <td class="px-2 py-2 text-xs text-slate-500" :title="exact(r.created_at)">{{ ago(r.created_at) }}</td>
          <td class="px-2 py-2 text-xs text-slate-600">{{ r.rules }} rules · {{ r.profiles }} profiles</td>
          <td class="px-4 py-2 text-right">
            <button class="mr-3 text-xs text-slate-500 hover:text-slate-200" @click="view(r)">view</button>
            <button
              v-if="r.id !== history.current"
              class="text-xs text-amber-300/80 hover:text-amber-200"
              @click="confirmDelete = { kind: 'restore', name: `revision ${r.id}`, revision: r }"
            >
              restore
            </button>
          </td>
        </tr>
      </table>
    </section>

    <!-- Import / export -->
    <section v-else-if="tab === 'transfer'" class="space-y-5">
      <div class="flex flex-wrap items-center gap-3">
        <p class="flex-1 text-sm text-slate-500">
          Download the running policy for review or backup, or replace it with one — JSON from an export, or a TOML
          policy file from an earlier release.
        </p>
        <a :href="api.exportUrl('json')" class="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-300 hover:border-slate-500">Export JSON</a>
        <a :href="api.exportUrl('toml')" class="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-300 hover:border-slate-500">Export TOML</a>
      </div>

      <div class="space-y-2">
        <div class="flex items-center gap-3">
          <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-400">Import</h3>
          <input type="file" accept=".json,.toml,application/json" class="text-xs text-slate-400" @change="readFile" />
        </div>
        <textarea
          v-model="importText"
          rows="12"
          spellcheck="false"
          placeholder="…or paste a policy here"
          class="w-full rounded-lg border border-slate-800 bg-slate-950 p-3 font-mono text-[12px] text-slate-200 outline-none focus:border-slate-600"
          @input="importCheck = null"
        ></textarea>
        <div class="flex items-center gap-3">
          <button class="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-300 hover:border-slate-500" :disabled="importing || !importText" @click="checkImport">
            {{ importing ? "Checking…" : "Check" }}
          </button>
          <button
            class="rounded-lg bg-amber-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-amber-300 disabled:opacity-40"
            :disabled="!importCheck?.ok || saving"
            @click="doImport"
          >
            Replace the running policy
          </button>
        </div>
        <div v-if="importCheck && !importCheck.ok" class="rounded-lg border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-xs text-rose-200">
          <div v-for="p in importCheck.problems" :key="p">{{ p }}</div>
        </div>
        <p v-else-if="importCheck?.ok" class="text-xs text-emerald-300">
          Valid: {{ importCheck.rules }} rules, {{ importCheck.profiles }} profiles.
          <template v-if="importCheck.impact">
            {{ importCheck.impact.changes }} of {{ importCheck.impact.hosts }} known machine(s) would boot something different.
          </template>
        </p>
      </div>
    </section>

    <!-- Dialogs -->
    <RuleWizard
      v-if="editingRule !== undefined"
      ref="wizard"
      :rule="editingRule"
      :seed="ruleSeed"
      :policy="policy"
      :saving="saving"
      @close="editingRule = undefined"
      @save="saveRule"
      @new-profile="profileFromWizard"
    />

    <ProfileWizard
      v-if="editingProfile !== undefined"
      :profile="editingProfile"
      :profiles="policy.profiles"
      :saving="saving"
      @close="(editingProfile = undefined), (profileForWizard = false)"
      @save="saveProfile"
    />

    <Modal v-if="viewing" wide :title="`Revision ${viewing.id}`" :subtitle="viewing.summary" @close="viewing = null">
      <pre class="max-h-[32rem] overflow-auto rounded-lg border border-slate-800 bg-slate-950 p-3 font-mono text-[11px] text-slate-300">{{ JSON.stringify(viewing.document, null, 2) }}</pre>
    </Modal>

    <Modal v-if="confirmDelete" :title="confirmDelete.kind === 'restore' ? `Restore ${confirmDelete.name}?` : `Delete the ${confirmDelete.kind} ${confirmDelete.name}?`" @close="confirmDelete = null">
      <p class="text-sm text-slate-300">
        <template v-if="confirmDelete.kind === 'restore'">
          The whole policy goes back to how it was after “{{ confirmDelete.revision.summary }}”. The current version stays in
          the history.
        </template>
        <template v-else>The version before this change stays in the history, so it can be restored.</template>
      </p>
      <p v-if="confirmDelete.kind === 'profile' && (confirmDelete.profile.used_by.length || confirmDelete.profile.in_menus.length || confirmDelete.profile.is_default)" class="mt-2 text-xs text-amber-300/80">
        Still in use — the delete will be refused until nothing points at it.
      </p>
      <template #actions>
        <button class="rounded-lg px-3 py-1.5 text-sm text-slate-400 hover:text-slate-200" @click="confirmDelete = null">Cancel</button>
        <button
          class="rounded-lg px-3 py-1.5 text-sm font-semibold text-slate-950"
          :class="confirmDelete.kind === 'restore' ? 'bg-amber-400 hover:bg-amber-300' : 'bg-rose-400 hover:bg-rose-300'"
          @click="
            confirmDelete.kind === 'restore'
              ? restore(confirmDelete.revision)
              : act(
                  () => (confirmDelete.kind === 'rule' ? api.removeRule(confirmDelete.name) : api.removeProfile(confirmDelete.name)),
                  `${confirmDelete.name} removed.`,
                ).then(() => (confirmDelete = null))
          "
        >
          {{ confirmDelete.kind === "restore" ? "Restore" : "Delete" }}
        </button>
      </template>
    </Modal>
  </div>
</template>
