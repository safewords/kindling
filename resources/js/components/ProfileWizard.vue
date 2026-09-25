<script setup>
import { computed, ref, watch } from "vue";
import Modal from "./Modal.vue";
import { api } from "../lib/api.js";
import { clone, schema } from "../lib/schema.js";

/**
 * A profile — the thing a machine is sent to boot — as a form per kind, with
 * the iPXE it would produce rendered beside it as you type.
 *
 * The form is chosen by kind rather than showing every field at once: a menu
 * has no kernel and a kernel has no menu, and a form offering both is a form
 * that invites a profile that is neither.
 */
const props = defineProps({
  /** `{ name, body }` to edit, or null for a new profile. */
  profile: { type: Object, default: null },
  profiles: { type: Array, default: () => [] },
  saving: { type: Boolean, default: false },
});
const emit = defineEmits(["close", "save"]);

const isNew = computed(() => !props.profile);
const name = ref("");
const body = ref({});
const kind = ref("kernel");
const step = ref("kind");

watch(
  () => props.profile,
  (profile) => {
    name.value = profile?.name || "";
    body.value = clone(profile?.body || {});
    body.value.initrd ||= [];
    body.value.entries ||= [];
    body.value.bootfile ||= {};
    kind.value = profile?.kind || "kernel";
    step.value = profile ? "details" : "kind";
  },
  { immediate: true },
);

function chooseKind(value) {
  kind.value = value;
  if (value === "kernel" && !body.value.kernel) body.value.kernel = "{{boot}}/images/…/vmlinuz";
  if (value === "script" && !body.value.script) body.value.script = "#!ipxe\necho Hello from {{mac}}\nshell\n";
  if (value === "menu" && !body.value.entries.length) {
    body.value.entries = props.profiles.slice(0, 2).map((p) => ({ profile: p.name }));
    body.value.timeout ??= 30;
  }
  step.value = "details";
}

/** Only the fields this kind uses, so a leftover kernel never rides along on a menu. */
const payload = computed(() => {
  const b = body.value;
  const out = { kind: kind.value };
  if (b.label?.trim()) out.label = b.label.trim();
  if (b.description?.trim()) out.description = b.description.trim();
  if (kind.value === "kernel") {
    out.kernel = b.kernel;
    const initrd = (b.initrd || []).filter((i) => i.trim());
    if (initrd.length) out.initrd = initrd;
    if (b.cmdline?.trim()) out.cmdline = b.cmdline.trim();
  }
  if (kind.value === "script") out.script = b.script;
  if (kind.value === "menu") {
    out.entries = (b.entries || [])
      .filter((e) => e.profile)
      .map((e) => Object.fromEntries(Object.entries(e).filter(([, v]) => v !== "" && v != null)));
    if (b.timeout !== "" && b.timeout != null) out.timeout = Number(b.timeout);
    if (b.default) out.default = b.default;
  }
  const bootfile = Object.fromEntries(Object.entries(b.bootfile || {}).filter(([k, v]) => k && v));
  if (Object.keys(bootfile).length) out.bootfile = bootfile;
  return out;
});

// --- the preview ---
const preview = ref({ script: "", problems: [], ok: true });
const previewMac = ref("");
let timer = null;
watch(
  [payload, name, previewMac],
  () => {
    clearTimeout(timer);
    if (step.value === "kind") return;
    timer = setTimeout(async () => {
      try {
        preview.value = await api.renderProfile({
          profile: payload.value,
          name: name.value || "preview",
          mac: previewMac.value || undefined,
        });
      } catch (e) {
        preview.value = { script: "", problems: [e.message], ok: false };
      }
    }, 300);
  },
  { deep: true, immediate: true },
);

// --- boot file overrides ---
const bootfileRows = computed(() => Object.entries(body.value.bootfile || {}));
const newBootArch = ref("");
function addBootfile() {
  const arch = newBootArch.value || "default";
  body.value.bootfile = { ...body.value.bootfile, [arch]: "" };
  newBootArch.value = "";
}
function removeBootfile(arch) {
  const next = { ...body.value.bootfile };
  delete next[arch];
  body.value.bootfile = next;
}

const nameOk = computed(() => /^[A-Za-z0-9._-]+$/.test(name.value.trim()));
const canSave = computed(() => nameOk.value && preview.value.ok && !props.saving);

function insert(field, placeholder) {
  body.value[field] = `${body.value[field] || ""}{{${placeholder}}}`;
}

function save() {
  const out = { ...payload.value };
  const original = props.profile?.name;
  if (original && name.value.trim() !== original) out.rename_to = name.value.trim();
  emit("save", { name: original || name.value.trim(), body: out });
}

const input =
  "mt-1 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-xs text-slate-200 outline-none focus:border-sky-500/60";
const label = "block text-xs uppercase tracking-wide text-slate-500";
const others = computed(() => props.profiles.filter((p) => p.name !== props.profile?.name));
</script>

<template>
  <Modal
    wide
    :title="isNew ? 'New profile' : `Profile: ${profile.name}`"
    subtitle="What a machine is sent to boot. The iPXE it produces is rendered as you type."
    @close="emit('close')"
  >
    <!-- Kind -->
    <section v-if="step === 'kind'" class="grid gap-2 sm:grid-cols-2">
      <button
        v-for="k in schema.profile_kinds"
        :key="k.kind"
        class="rounded-lg border px-4 py-3 text-left hover:border-sky-500/50 hover:bg-sky-500/5"
        :class="kind === k.kind ? 'border-sky-500/60' : 'border-slate-800'"
        @click="chooseKind(k.kind)"
      >
        <div class="text-sm font-medium text-slate-100">{{ k.label }}</div>
        <div class="mt-0.5 text-xs text-slate-500">{{ k.help }}</div>
      </button>
    </section>

    <section v-else class="grid gap-5 lg:grid-cols-2">
      <div class="space-y-4">
        <div class="flex items-center gap-2 text-xs">
          <span class="rounded-full border border-slate-700 px-2 py-0.5 text-slate-300">
            {{ schema.profile_kinds.find((k) => k.kind === kind)?.label || kind }}
          </span>
          <button class="text-sky-300 hover:text-sky-200" @click="step = 'kind'">change kind</button>
        </div>

        <div class="grid gap-3 sm:grid-cols-2">
          <div>
            <label :class="label">Name</label>
            <input v-model="name" placeholder="ubuntu-2404" :class="input" />
            <p v-if="name && !nameOk" class="mt-1 text-[11px] text-rose-300">Letters, digits, - _ and . only: it goes into a URL.</p>
            <p v-else-if="!isNew && name !== profile.name" class="mt-1 text-[11px] text-amber-300/80">
              Renaming updates every rule and menu that points at it.
            </p>
          </div>
          <div>
            <label :class="label">Label</label>
            <input v-model="body.label" placeholder="Ubuntu 24.04 — unattended install" :class="[input, 'font-sans']" />
          </div>
        </div>
        <div>
          <label :class="label">Description</label>
          <input v-model="body.description" placeholder="Wipes the disk." :class="[input, 'font-sans']" />
        </div>

        <!-- kernel -->
        <template v-if="kind === 'kernel'">
          <div>
            <label :class="label">Kernel</label>
            <input v-model="body.kernel" :class="input" />
          </div>
          <div>
            <div class="flex items-baseline justify-between">
              <label :class="label">Initrds</label>
              <button class="text-xs text-sky-300" @click="body.initrd.push('')">+ initrd</button>
            </div>
            <div v-for="(_, i) in body.initrd" :key="i" class="flex gap-2">
              <input v-model="body.initrd[i]" :class="input" />
              <button class="mt-1 text-xs text-slate-600 hover:text-rose-400" @click="body.initrd.splice(i, 1)">✕</button>
            </div>
          </div>
          <div>
            <label :class="label">Kernel command line</label>
            <textarea v-model="body.cmdline" rows="3" :class="input"></textarea>
          </div>
        </template>

        <!-- script -->
        <div v-else-if="kind === 'script'">
          <label :class="label">iPXE script</label>
          <textarea v-model="body.script" rows="12" spellcheck="false" :class="input"></textarea>
        </div>

        <!-- menu -->
        <template v-else-if="kind === 'menu'">
          <div>
            <div class="flex items-baseline justify-between">
              <label :class="label">Entries</label>
              <button class="text-xs text-sky-300" @click="body.entries.push({ profile: '' })">+ entry</button>
            </div>
            <div v-for="(entry, i) in body.entries" :key="i" class="mt-1 flex items-center gap-2">
              <select v-model="entry.profile" class="flex-1 rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs outline-none">
                <option value="">— profile —</option>
                <option v-for="p in others" :key="p.name" :value="p.name">{{ p.name }}</option>
              </select>
              <input v-model="entry.label" placeholder="label" class="w-32 rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs outline-none" />
              <input v-model="entry.key" placeholder="key" maxlength="1" class="w-12 rounded border border-slate-700 bg-slate-950 px-2 py-1 text-center font-mono text-xs outline-none" />
              <button class="text-xs text-slate-600 hover:text-rose-400" @click="body.entries.splice(i, 1)">✕</button>
            </div>
          </div>
          <div class="grid gap-3 sm:grid-cols-2">
            <div>
              <label :class="label">Default</label>
              <select v-model="body.default" :class="input">
                <option :value="undefined">the first entry</option>
                <option v-for="e in body.entries.filter((e) => e.profile)" :key="e.profile" :value="e.profile">{{ e.profile }}</option>
              </select>
            </div>
            <div>
              <label :class="label">Timeout (seconds)</label>
              <input v-model.number="body.timeout" type="number" placeholder="wait forever" :class="input" />
            </div>
          </div>
        </template>

        <p v-else class="text-sm text-slate-400">
          {{ schema.profile_kinds.find((k) => k.kind === kind)?.help }} Nothing else to fill in.
        </p>

        <div v-if="kind === 'kernel' || kind === 'script'" class="text-[11px] text-slate-600">
          Placeholders:
          <button
            v-for="p in schema.placeholders"
            :key="p"
            class="mr-1 rounded bg-slate-800 px-1 font-mono text-slate-400 hover:text-slate-100"
            @click="insert(kind === 'kernel' ? 'cmdline' : 'script', p === 'var.<name>' ? 'var.role' : p)"
          >
            {{ p }}
          </button>
          <span class="block mt-1">iPXE's own <code v-pre>${…}</code> variables pass through untouched.</span>
        </div>

        <details class="rounded-lg border border-slate-800 px-3 py-2">
          <summary class="cursor-pointer text-xs text-slate-400">Hand this profile's machines a different stage-one loader</summary>
          <p class="mt-2 text-[11px] text-slate-600">
            For the rare machine that should get `pxelinux.0` or a vendor loader instead of iPXE, by architecture.
          </p>
          <div v-for="[arch] in bootfileRows" :key="arch" class="mt-1 flex items-center gap-2">
            <code class="w-28 text-xs text-slate-400">{{ arch }}</code>
            <input v-model="body.bootfile[arch]" placeholder="pxelinux.0" class="flex-1 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs outline-none" />
            <button class="text-xs text-slate-600 hover:text-rose-400" @click="removeBootfile(arch)">✕</button>
          </div>
          <div class="mt-2 flex gap-2">
            <select v-model="newBootArch" class="rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs outline-none">
              <option value="">default</option>
              <option v-for="a in schema.architectures" :key="a" :value="a">{{ a }}</option>
            </select>
            <button class="text-xs text-sky-300" @click="addBootfile">+ loader</button>
          </div>
        </details>
      </div>

      <!-- preview -->
      <div class="space-y-2">
        <div class="flex items-center gap-2">
          <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-400">The iPXE it produces</h3>
          <input
            v-model="previewMac"
            placeholder="for a MAC… (optional)"
            class="ml-auto w-44 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-[11px] outline-none"
          />
        </div>
        <pre class="max-h-[28rem] min-h-40 overflow-auto rounded-lg border border-slate-800 bg-slate-950 p-3 font-mono text-[11px] text-slate-300">{{ preview.script || "—" }}</pre>
        <div v-if="preview.problems?.length" class="rounded-lg border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-xs text-rose-200">
          <div v-for="p in preview.problems" :key="p">{{ p }}</div>
        </div>
      </div>
    </section>

    <template #actions>
      <button class="rounded-lg px-3 py-1.5 text-sm text-slate-400 hover:text-slate-200" @click="emit('close')">Cancel</button>
      <button
        class="rounded-lg bg-sky-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-sky-300 disabled:opacity-40"
        :disabled="!canSave || step === 'kind'"
        @click="save"
      >
        {{ saving ? "Saving…" : "Save profile" }}
      </button>
    </template>
  </Modal>
</template>
