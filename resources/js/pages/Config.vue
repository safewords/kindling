<script setup>
import { computed, onMounted, ref, watch } from "vue";
import DocEditor from "../components/DocEditor.vue";
import { api } from "../lib/api.js";
import { useLive } from "../lib/live.js";
import { notify } from "../lib/toast.js";

const config = ref(null);
const loading = ref(true);
const saving = ref(false);
const tab = ref("settings");

/** Values the operator has changed but not saved. */
const edits = ref({});
const source = ref("");
const checked = ref({ ok: false, problems: [], checking: false });
let checkTimer = null;

/** `.env` was written elsewhere while this screen held unsaved work. */
const stale = ref(false);

/** `quiet` keeps what is on screen while it loads — for a refresh nobody asked for. */
async function load({ quiet = false } = {}) {
  if (!quiet) loading.value = true;
  try {
    config.value = await api.config();
    source.value = config.value.text;
    edits.value = {};
    stale.value = false;
    checked.value = { ok: false, problems: [], checking: false };
  } catch (e) {
    notify.error(e);
  } finally {
    loading.value = false;
  }
}

onMounted(load);

const sections = computed(() => {
  if (!config.value) return [];
  return config.value.sections.map((section) => ({
    name: section.name,
    settings: config.value.settings.filter((s) => s.section === section.name),
  }));
});

/** The value a control shows: the pending edit, else the file, else nothing. */
const shown = (setting) =>
  setting.key in edits.value ? edits.value[setting.key] : (setting.redacted ? "" : setting.value ?? "");

const changed = (setting) =>
  setting.key in edits.value && edits.value[setting.key] !== (setting.value ?? "");

const pending = computed(() =>
  Object.entries(edits.value).filter(([key, value]) => {
    const setting = config.value.settings.find((s) => s.key === key);
    return value !== (setting?.value ?? "");
  }),
);

function edit(setting, value) {
  edits.value = { ...edits.value, [setting.key]: value };
}

async function saveSettings() {
  saving.value = true;
  try {
    const changes = Object.fromEntries(
      pending.value.map(([key, value]) => [key, value === "" ? null : String(value)]),
    );
    const result = await api.patchConfig(changes);
    notify.ok(
      `${result.changed.length} setting${result.changed.length === 1 ? "" : "s"} written.`,
      "Configuration is read at startup, so this takes effect when the server next starts.",
    );
    await load();
  } catch (e) {
    notify.error(e);
  } finally {
    saving.value = false;
  }
}

const dirtySource = computed(() => config.value && source.value !== config.value.text);

// The announcement carries only the names of what changed, so the values are
// fetched through the read API — which redacts the secret ones — exactly as
// on first load.
useLive({
  config: () => {
    if (saving.value) return;
    if (dirtySource.value || pending.value.length) stale.value = true;
    else load({ quiet: true });
  },
  resync: () => !saving.value && !dirtySource.value && !pending.value.length && load({ quiet: true }),
});

watch(source, () => {
  if (!dirtySource.value) {
    checked.value = { ok: false, problems: [], checking: false };
    return;
  }
  clearTimeout(checkTimer);
  checked.value.checking = true;
  checkTimer = setTimeout(async () => {
    try {
      const outcome = await api.validateConfig(source.value);
      checked.value = { ok: outcome.ok, problems: outcome.problems || [], checking: false };
    } catch (e) {
      checked.value = { ok: false, problems: [e.message], checking: false };
    }
  }, 350);
});

async function saveSource() {
  saving.value = true;
  try {
    await api.saveConfig(source.value);
    notify.ok("Written.", "Takes effect when the server next starts.");
    await load();
  } catch (e) {
    notify.error(e);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div v-if="loading" class="py-16 text-center text-slate-500">Loading…</div>

  <div v-else-if="config" class="space-y-5">
    <!-- Said once, at the top, rather than implied by a Save button that
         looks like it applied something. -->
    <div class="rounded-lg border border-slate-700/60 bg-slate-900/50 px-4 py-3 text-sm text-slate-400">
      Configuration is read <b class="text-slate-200">once, at startup</b>. Saving here writes
      <code class="font-mono">{{ config.path }}</code>; the server picks it up when it next
      starts. The policy file is the exception — that reloads on demand.
    </div>

    <!-- Somebody else changed the file while this screen had unsaved edits.
         Not reloaded underneath them — that would throw their work away — but
         not left unsaid either, because saving now would overwrite the other
         change without anyone having seen it. -->
    <div
      v-if="stale"
      class="flex flex-wrap items-center gap-3 rounded-lg border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm text-amber-100"
    >
      <div class="flex-1">
        The file was changed elsewhere while you were editing. Saving now would replace that
        change with yours.
      </div>
      <button
        class="rounded-md border border-amber-400/50 px-3 py-1.5 text-xs text-amber-100 hover:bg-amber-400/10"
        @click="load({ quiet: true })"
      >
        Discard mine and load theirs
      </button>
    </div>

    <div class="flex flex-wrap items-center gap-4">
      <div class="flex gap-1">
        <button
          class="rounded-lg px-3 py-1.5 text-sm"
          :class="tab === 'settings' ? 'bg-slate-800 text-slate-100' : 'text-slate-400 hover:text-slate-200'"
          @click="tab = 'settings'"
        >
          Settings
        </button>
        <button
          class="rounded-lg px-3 py-1.5 text-sm"
          :class="tab === 'source' ? 'bg-slate-800 text-slate-100' : 'text-slate-400 hover:text-slate-200'"
          @click="tab = 'source'"
        >
          Source
        </button>
      </div>

      <div v-if="tab === 'settings' && pending.length" class="ml-auto flex items-center gap-3">
        <span class="text-xs text-amber-300">
          {{ pending.length }} unsaved change{{ pending.length === 1 ? "" : "s" }}
        </span>
        <button class="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-400" @click="edits = {}">
          Discard
        </button>
        <button
          class="rounded-lg bg-sky-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-sky-300 disabled:opacity-50"
          :disabled="saving"
          @click="saveSettings"
        >
          {{ saving ? "Saving…" : "Save" }}
        </button>
      </div>
    </div>

    <section v-if="tab === 'settings'" class="space-y-6">
      <div v-for="section in sections" :key="section.name">
        <h2 class="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-500">
          {{ section.name }}
        </h2>

        <div class="divide-y divide-slate-800 overflow-hidden rounded-xl border border-slate-800">
          <div
            v-for="setting in section.settings"
            :key="setting.key"
            class="grid gap-3 px-4 py-3 sm:grid-cols-[18rem_1fr]"
            :class="changed(setting) ? 'bg-sky-500/5' : ''"
          >
            <div>
              <label class="text-sm text-slate-200">{{ setting.label }}</label>
              <code class="mt-0.5 block font-mono text-[11px] text-slate-600">{{ setting.key }}</code>
            </div>

            <div>
              <select
                v-if="setting.kind === 'boolean'"
                class="w-full max-w-xs rounded-lg border border-slate-700 bg-slate-950 px-3 py-1.5 text-sm outline-none focus:border-slate-500"
                :value="shown(setting)"
                @change="edit(setting, $event.target.value)"
              >
                <option value="">— default ({{ setting.default }}) —</option>
                <option value="true">true</option>
                <option value="false">false</option>
              </select>

              <select
                v-else-if="setting.kind === 'choice'"
                class="w-full max-w-xs rounded-lg border border-slate-700 bg-slate-950 px-3 py-1.5 text-sm outline-none focus:border-slate-500"
                :value="shown(setting)"
                @change="edit(setting, $event.target.value)"
              >
                <option value="">— default ({{ setting.default }}) —</option>
                <option v-for="choice in setting.choices" :key="choice" :value="choice">{{ choice }}</option>
              </select>

              <input
                v-else
                :type="setting.kind === 'secret' ? 'password' : setting.kind === 'integer' ? 'number' : 'text'"
                class="w-full max-w-md rounded-lg border border-slate-700 bg-slate-950 px-3 py-1.5 font-mono text-sm outline-none placeholder:text-slate-600 focus:border-slate-500"
                :value="shown(setting)"
                :placeholder="setting.redacted ? setting.value : setting.default"
                @input="edit(setting, $event.target.value)"
              />

              <p class="mt-1.5 text-xs leading-relaxed text-slate-500">{{ setting.help }}</p>

              <p v-if="setting.redacted" class="mt-1 text-[11px] text-slate-600">
                Set. Never shown in full — type a new value to replace it, or leave it alone.
              </p>
            </div>
          </div>
        </div>
      </div>

      <div class="rounded-xl border border-slate-800 bg-slate-900/40 px-4 py-3">
        <h2 class="text-xs font-semibold uppercase tracking-wider text-slate-500">
          What this process is actually running on
        </h2>
        <p class="mt-1 text-xs text-slate-600">
          Not the same as the file above once somebody has edited it without restarting — which is
          exactly when it is worth checking.
        </p>
        <dl class="mt-3 grid gap-x-8 gap-y-1.5 text-xs sm:grid-cols-2">
          <div v-for="(value, key) in config.running" :key="key" class="flex justify-between gap-4">
            <dt class="text-slate-500">{{ key.replace(/_/g, " ") }}</dt>
            <dd class="truncate font-mono text-slate-300">{{ String(value) }}</dd>
          </div>
        </dl>
      </div>
    </section>

    <section v-else class="space-y-3">
      <div class="flex flex-wrap items-center gap-3">
        <p class="flex-1 text-sm text-slate-500">
          The file itself. Checked as you type by the same <code class="font-mono">configure</code>
          the server boots with — you cannot save a file that will not start.
        </p>
        <button
          v-if="dirtySource"
          class="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-400"
          @click="source = config.text"
        >
          Discard
        </button>
        <button
          class="rounded-lg bg-sky-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-sky-300 disabled:opacity-40"
          :disabled="!dirtySource || !checked.ok || saving"
          @click="saveSource"
        >
          {{ saving ? "Saving…" : "Save" }}
        </button>
      </div>

      <DocEditor
        v-model="source"
        :problems="checked.problems"
        :ok="checked.ok && dirtySource"
        :checking="checked.checking"
      />
    </section>
  </div>
</template>
