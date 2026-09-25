<script setup>
import { computed, onMounted, ref, watch } from "vue";
import Modal from "./Modal.vue";
import ConditionBuilder from "./ConditionBuilder.vue";
import ValueInput from "./ValueInput.vue";
import { api } from "../lib/api.js";
import { always, clone, describe, normalise } from "../lib/schema.js";
import { notify } from "../lib/toast.js";

/**
 * A rule, built a step at a time: which machines, what happens to them, the
 * exceptions, where it sits among the other rules — and then, before the
 * button that saves it, which of the machines this server already knows it
 * would change.
 *
 * Every step is also a tab, so an existing rule opens straight into the one
 * that needs changing and nobody is walked through six screens to fix a typo.
 */
const props = defineProps({
  /** The rule to edit, or null for a new one. */
  rule: { type: Object, default: null },
  /** Start a new rule from this instead of blank (duplicate). */
  seed: { type: Object, default: null },
  policy: { type: Object, required: true },
  saving: { type: Boolean, default: false },
});
const emit = defineEmits(["close", "save", "new-profile"]);

const isNew = computed(() => !props.rule);
const STEPS = [
  ["start", "Start"],
  ["who", "Which machines"],
  ["what", "What happens"],
  ["except", "Exceptions"],
  ["order", "Name & order"],
  ["review", "Review"],
];
const step = ref(isNew.value && !props.seed ? "start" : "who");
const stepIndex = computed(() => STEPS.findIndex(([key]) => key === step.value));
const visibleSteps = computed(() => (isNew.value ? STEPS : STEPS.filter(([key]) => key !== "start")));

// --- the draft ---
const draft = ref(blank());

function blank() {
  return {
    name: "",
    description: "",
    enabled: true,
    priority: 0,
    when: always(),
    unless: null,
    profile: "",
    tag: [],
    remove_tags: [],
    set: [],
    stop: null,
  };
}

/** A stored rule as the wizard's working copy. */
function toDraft(rule) {
  const r = clone(rule || {});
  return {
    ...blank(),
    ...r,
    description: r.description || "",
    profile: r.profile || "",
    tag: r.tag || [],
    remove_tags: r.remove_tags || [],
    set: Object.entries(r.set || {}).map(([key, value]) => ({ key, value })),
    when: r.when || always(),
    unless: r.unless || null,
    stop: r.stop ?? null,
  };
}

/** The working copy as the API takes it. */
const payload = computed(() => {
  const d = draft.value;
  const rule = {
    name: d.name.trim(),
    enabled: d.enabled,
    priority: Number(d.priority) || 0,
    when: normalise(d.when),
  };
  if (d.description.trim()) rule.description = d.description.trim();
  if (d.unless && (d.unless.all?.length || d.unless.any?.length)) rule.unless = normalise(d.unless);
  if (d.profile) rule.profile = d.profile;
  if (d.tag.length) rule.tag = d.tag;
  if (d.remove_tags.length) rule.remove_tags = d.remove_tags;
  const vars = d.set.filter((v) => v.key.trim());
  if (vars.length) rule.set = Object.fromEntries(vars.map((v) => [v.key.trim(), v.value]));
  if (d.stop !== null) rule.stop = d.stop;
  return rule;
});

watch(
  () => [props.rule, props.seed],
  () => {
    draft.value = toDraft(props.rule || props.seed);
    if (props.seed && !props.rule) draft.value.name = `${props.seed.name || "rule"}-copy`;
  },
  { immediate: true },
);

// --- starters ---
const templates = ref([]);
onMounted(async () => {
  try {
    templates.value = (await api.templates()).data;
  } catch {
    templates.value = [];
  }
});

const t = (fact, op, value) => (value === undefined ? { fact, op } : { fact, op, value });
const STARTERS = [
  {
    name: "Blank rule",
    description: "Start from nothing and build every condition yourself.",
    rule: {},
  },
  {
    name: "Install new machines",
    description: "Anything that has never booted here gets an installer — once. A `no-install` tag opts a machine out.",
    rule: {
      when: { all: [t("known", "is", false), t("device_class", "in", ["physical", "virtual"])] },
      unless: { all: [t("tag", "has_any", ["no-install"])] },
      tag: ["provisioned"],
      priority: 100,
    },
  },
  {
    name: "A hardware model",
    description: "Match on the SMBIOS product name iPXE reports, e.g. `PowerEdge R6*`.",
    rule: { when: { all: [t("product", "glob", [])] } },
  },
  {
    name: "A vendor",
    description: "Everything from one manufacturer, by the address's OUI.",
    rule: { when: { all: [t("vendor", "glob", [])] } },
  },
  {
    name: "Specific machines",
    description: "A hand-picked list of MAC addresses.",
    rule: { when: { all: [t("mac", "in", [])] } },
  },
  {
    name: "A network",
    description: "Everything on a subnet — the relay's, if the request was relayed.",
    rule: { when: { all: [t("network", "in_subnet", [])] } },
  },
  {
    name: "Tagged machines",
    description: "Machines an operator has tagged in the inventory.",
    rule: { when: { all: [t("tag", "has_any", [])] } },
  },
  {
    name: "Reimage overnight",
    description: "Machines tagged `reimage`, but only inside a quiet window.",
    rule: {
      when: { all: [t("tag", "has_any", ["reimage"]), t("time", "between", ["22:00", "06:00"])] },
      priority: 90,
    },
  },
  {
    name: "Give up after N tries",
    description: "A machine that has failed several times gets the menu instead of another attempt.",
    rule: { when: { all: [t("tag", "has_any", ["reimage"]), t("boot_count", "gte", 3)] }, priority: 80 },
  },
  {
    name: "Classify, don't boot",
    description: "Set a variable and a tag that later rules and profile templates can use.",
    rule: { when: { all: [t("product", "glob", [])] }, set: { role: "" }, priority: 500 },
  },
  {
    name: "Leave hardware alone",
    description: "Switches and access points PXE boot too. Point this at a `do not answer` profile.",
    rule: { when: { all: [t("device_class", "in", ["network"])] }, priority: 1000 },
  },
  {
    name: "Catch-all",
    description: "No conditions, lowest priority: what everything else boots.",
    rule: { priority: -100 },
  },
];

function start(template) {
  const name = draft.value.name;
  draft.value = toDraft({ ...template.rule, name: template.rule.name || name });
  step.value = "who";
}

// --- actions ---
const profiles = computed(() => props.policy.profiles || []);
const chosenProfile = computed(() => profiles.value.find((p) => p.name === draft.value.profile));
const stopsByDefault = computed(() => Boolean(draft.value.profile));
const addVar = () => draft.value.set.push({ key: "", value: "" });

/** Called by the parent when a profile was created from inside the wizard. */
function useProfile(name) {
  draft.value.profile = name;
}
defineExpose({ useProfile });

// --- exceptions and timing ---
const hasUnless = computed({
  get: () => Boolean(draft.value.unless),
  set: (on) => (draft.value.unless = on ? { any: [t("tag", "has_any", ["hold"])] } : null),
});

function addToWhen(test) {
  const root = draft.value.when;
  if ("all" in root) root.all.push(test);
  else draft.value.when = { all: [root, test] };
}

// --- ordering ---
const ordered = computed(() => props.policy.rules.filter((r) => r.name !== props.rule?.name));
function placeAbove(name) {
  const index = ordered.value.findIndex((r) => r.name === name);
  if (index < 0) return;
  const target = ordered.value[index].priority;
  const above = index > 0 ? ordered.value[index - 1].priority : target + 20;
  draft.value.priority = above - target > 1 ? Math.floor((above + target) / 2) : target + 1;
}

// --- live checking ---
const check = ref({ ok: null, problems: [], focus: null, checking: false });
let timer = null;
watch(
  payload,
  () => {
    clearTimeout(timer);
    check.value.checking = true;
    timer = setTimeout(async () => {
      if (!payload.value.name) {
        check.value = { ok: false, problems: ["The rule needs a name (step “Name & order”)."], focus: null, checking: false };
        return;
      }
      try {
        const outcome = await api.validatePolicy({ rule: payload.value, original: props.rule?.name });
        check.value = { ok: outcome.ok, problems: outcome.problems || [], focus: outcome.focus, checking: false };
      } catch (e) {
        check.value = { ok: false, problems: [e.message], focus: null, checking: false };
      }
    }, 300);
  },
  { deep: true, immediate: true },
);

// --- impact ---
const impact = ref(null);
const previewing = ref(false);
async function runPreview() {
  if (!payload.value.name) return;
  previewing.value = true;
  try {
    impact.value = await api.previewPolicy({ rule: payload.value, original: props.rule?.name });
  } catch (e) {
    notify.error(e);
  } finally {
    previewing.value = false;
  }
}
watch(step, (now) => now === "review" && runPreview());

// --- saving ---
const canSave = computed(() => check.value.ok && !check.value.checking && !props.saving);
const next = () => (step.value = STEPS[Math.min(stepIndex.value + 1, STEPS.length - 1)][0]);
const back = () => (step.value = STEPS[Math.max(stepIndex.value - 1, isNew.value ? 0 : 1)][0]);

const savingTemplate = ref(false);
async function saveAsTemplate() {
  const name = window.prompt("Save these conditions and actions as a wizard starter called:", draft.value.name || "");
  if (!name) return;
  savingTemplate.value = true;
  try {
    const { name: _ignored, ...rule } = payload.value;
    await api.saveTemplate({ name, description: draft.value.description || undefined, category: "saved", rule });
    templates.value = (await api.templates()).data;
    notify.ok(`Saved the starter “${name}”.`);
  } catch (e) {
    notify.error(e);
  } finally {
    savingTemplate.value = false;
  }
}

async function forgetTemplate(template) {
  try {
    await api.deleteTemplate(template.id);
    templates.value = templates.value.filter((x) => x.id !== template.id);
  } catch (e) {
    notify.error(e);
  }
}

const input =
  "mt-1 w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 outline-none focus:border-sky-500/60";
const label = "block text-xs uppercase tracking-wide text-slate-500";
</script>

<template>
  <Modal
    wide
    :title="isNew ? 'New rule' : `Rule: ${rule.name}`"
    subtitle="Built step by step; checked against the running policy as you go. Nothing is stored until you save."
    @close="emit('close')"
  >
    <!-- Steps -->
    <nav class="-mt-1 mb-5 flex flex-wrap gap-1">
      <button
        v-for="([key, title], i) in visibleSteps"
        :key="key"
        class="flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs transition-colors"
        :class="
          step === key
            ? 'border-sky-500/60 bg-sky-500/15 text-sky-100'
            : 'border-slate-700 text-slate-400 hover:border-slate-500 hover:text-slate-200'
        "
        @click="step = key"
      >
        <span class="font-mono text-[10px] text-slate-500">{{ i + 1 }}</span>
        {{ title }}
      </button>
    </nav>

    <!-- 1. Start -->
    <section v-if="step === 'start'" class="space-y-4">
      <p class="text-sm text-slate-400">Pick a starting point. Everything it fills in can be changed on the next steps.</p>
      <div class="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
        <button
          v-for="starter in STARTERS"
          :key="starter.name"
          class="rounded-lg border border-slate-800 bg-slate-950/40 px-3 py-2.5 text-left hover:border-sky-500/50 hover:bg-sky-500/5"
          @click="start(starter)"
        >
          <div class="text-sm font-medium text-slate-100">{{ starter.name }}</div>
          <div class="mt-0.5 text-xs text-slate-500">{{ starter.description }}</div>
        </button>
      </div>

      <div v-if="templates.length">
        <h3 class="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-400">Your saved starters</h3>
        <div class="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
          <div
            v-for="template in templates"
            :key="template.id"
            class="group relative rounded-lg border border-slate-800 bg-slate-950/40 hover:border-emerald-500/50"
          >
            <button class="w-full px-3 py-2.5 text-left" @click="start(template)">
              <div class="text-sm font-medium text-slate-100">{{ template.name }}</div>
              <div class="mt-0.5 text-xs text-slate-500">
                {{ template.description || describe(template.rule.when) }}
              </div>
            </button>
            <button
              class="absolute right-2 top-2 hidden text-xs text-slate-600 hover:text-rose-400 group-hover:block"
              title="Forget this starter"
              @click="forgetTemplate(template)"
            >
              ✕
            </button>
          </div>
        </div>
      </div>
    </section>

    <!-- 2. Which machines -->
    <section v-else-if="step === 'who'" class="space-y-3">
      <p class="text-sm text-slate-400">
        Which machines this rule is about. Combine conditions with <b>all of</b> / <b>any of</b> groups, and
        negate any of them with <b>not</b>.
      </p>
      <ConditionBuilder v-model="draft.when" empty-hint="every machine — a catch-all" />
    </section>

    <!-- 3. What happens -->
    <section v-else-if="step === 'what'" class="space-y-5">
      <div>
        <label :class="label">Boot</label>
        <div class="mt-1 flex gap-2">
          <select v-model="draft.profile" :class="[input, 'mt-0']">
            <option value="">— nothing: this rule only tags or sets variables —</option>
            <option v-for="p in profiles" :key="p.name" :value="p.name">{{ p.name }} — {{ p.label }} ({{ p.kind }})</option>
          </select>
          <button
            class="shrink-0 rounded-lg border border-slate-700 px-3 text-xs text-slate-300 hover:border-slate-500"
            @click="emit('new-profile')"
          >
            New profile…
          </button>
        </div>
        <p v-if="chosenProfile?.body?.description" class="mt-1 text-xs text-amber-300/80">
          {{ chosenProfile.body.description }}
        </p>
      </div>

      <div class="grid gap-4 sm:grid-cols-2">
        <div>
          <label :class="label">Add tags</label>
          <div class="mt-1 flex"><ValueInput v-model="draft.tag" fact="tag" op="has_any" /></div>
          <p class="mt-1 text-[11px] text-slate-600">Kept on the machine in the inventory, and seen by every later rule.</p>
        </div>
        <div>
          <label :class="label">Remove tags</label>
          <div class="mt-1 flex"><ValueInput v-model="draft.remove_tags" fact="tag" op="has_any" /></div>
          <p class="mt-1 text-[11px] text-slate-600">Taken off the machine — e.g. clear `reimage` once it has been done.</p>
        </div>
      </div>

      <div>
        <div class="flex items-baseline justify-between">
          <label :class="label">Set variables</label>
          <button class="text-xs text-sky-300 hover:text-sky-200" @click="addVar">+ variable</button>
        </div>
        <p class="mt-1 text-[11px] text-slate-600">
          Later rules can test them (fact <i>Variable</i>), and profiles can use them as
          <code class="text-slate-400" v-pre>{{ var.name }}</code> in a kernel line or script.
        </p>
        <div v-for="(v, i) in draft.set" :key="i" class="mt-2 flex items-center gap-2">
          <input v-model="v.key" placeholder="role" class="w-40 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs outline-none" />
          <span class="text-slate-600">=</span>
          <input v-model="v.value" placeholder="storage" class="flex-1 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs outline-none" />
          <button class="text-xs text-slate-600 hover:text-rose-400" @click="draft.set.splice(i, 1)">✕</button>
        </div>
      </div>

      <div>
        <label :class="label">After this rule fires</label>
        <div class="mt-1 flex flex-wrap gap-2 text-xs">
          <button
            v-for="[value, text] in [[null, `default (${stopsByDefault ? 'stop' : 'carry on'})`], [true, 'stop evaluating'], [false, 'carry on to later rules']]"
            :key="String(value)"
            class="rounded-lg border px-3 py-1.5"
            :class="draft.stop === value ? 'border-sky-500/60 bg-sky-500/15 text-sky-100' : 'border-slate-700 text-slate-400 hover:border-slate-500'"
            @click="draft.stop = value"
          >
            {{ text }}
          </button>
        </div>
        <p class="mt-1 text-[11px] text-slate-600">
          A rule that chooses a profile stops by default, and one that only tags carries on so tags compose. The first
          rule to choose a profile wins either way.
        </p>
      </div>
    </section>

    <!-- 4. Exceptions and timing -->
    <section v-else-if="step === 'except'" class="space-y-5">
      <div>
        <label class="flex items-center gap-2 text-sm text-slate-200">
          <input v-model="hasUnless" type="checkbox" class="accent-sky-400" />
          Except when…
        </label>
        <p class="mt-1 text-xs text-slate-500">
          If this holds, the rule does not fire even though the machine matched — “every Dell except the two in the
          corner”. The test trace calls it <i>excluded</i>.
        </p>
        <div v-if="draft.unless" class="mt-3">
          <ConditionBuilder v-model="draft.unless" />
        </div>
      </div>

      <div>
        <h3 :class="label">Only at certain times</h3>
        <p class="mt-1 text-xs text-slate-500">
          Adds a condition to “Which machines”. Times are UTC shifted by the policy's timezone offset
          ({{ policy.settings.timezone_offset_minutes || 0 }} minutes).
        </p>
        <div class="mt-2 flex flex-wrap gap-2 text-xs">
          <button class="rounded-lg border border-slate-700 px-3 py-1.5 text-slate-300 hover:border-slate-500" @click="addToWhen(t('time', 'between', ['22:00', '06:00']))">
            + overnight window
          </button>
          <button class="rounded-lg border border-slate-700 px-3 py-1.5 text-slate-300 hover:border-slate-500" @click="addToWhen(t('weekday', 'in', ['weekend']))">
            + weekends only
          </button>
          <button class="rounded-lg border border-slate-700 px-3 py-1.5 text-slate-300 hover:border-slate-500" @click="addToWhen(t('weekday', 'in', ['weekday']))">
            + weekdays only
          </button>
          <button class="rounded-lg border border-slate-700 px-3 py-1.5 text-slate-300 hover:border-slate-500" @click="addToWhen(t('boot_count', 'lte', 2))">
            + at most N attempts
          </button>
        </div>
        <p class="mt-2 text-xs text-slate-500">Currently: {{ describe(draft.when) }}</p>
      </div>
    </section>

    <!-- 5. Name and order -->
    <section v-else-if="step === 'order'" class="space-y-4">
      <div class="grid gap-4 sm:grid-cols-2">
        <div>
          <label :class="label">Name</label>
          <input v-model="draft.name" placeholder="image-new-machines" :class="[input, 'font-mono']" />
          <p class="mt-1 text-[11px] text-slate-600">What the boot log says fired — make it something somebody can act on.</p>
        </div>
        <div>
          <label :class="label">Priority</label>
          <input v-model.number="draft.priority" type="number" :class="[input, 'font-mono']" />
          <p class="mt-1 text-[11px] text-slate-600">Higher runs first; ties keep the order they were added in.</p>
        </div>
      </div>
      <div>
        <label :class="label">Why this rule exists</label>
        <input v-model="draft.description" placeholder="Anything that has never booted here gets provisioned." :class="input" />
      </div>
      <label class="flex items-center gap-2 text-sm text-slate-300">
        <input v-model="draft.enabled" type="checkbox" class="accent-sky-400" />
        Enabled
      </label>

      <div class="rounded-lg border border-slate-800">
        <div class="border-b border-slate-800 px-3 py-2 text-xs text-slate-500">
          Evaluation order — click a rule to run just before it.
        </div>
        <div class="max-h-64 overflow-y-auto">
          <template v-for="(r, i) in ordered" :key="r.name">
            <div
              v-if="check.focus && check.focus.position === i"
              class="border-y border-sky-500/40 bg-sky-500/10 px-3 py-1.5 font-mono text-xs text-sky-200"
            >
              {{ draft.priority }} · {{ draft.name || "this rule" }} ← here
            </div>
            <button
              class="flex w-full items-baseline gap-3 px-3 py-1 text-left font-mono text-xs hover:bg-slate-800/60"
              :class="r.enabled ? 'text-slate-300' : 'text-slate-600'"
              @click="placeAbove(r.name)"
            >
              <span class="w-12 text-right text-slate-600">{{ r.priority }}</span>
              {{ r.name }}
            </button>
          </template>
          <div
            v-if="check.focus && check.focus.position >= ordered.length"
            class="border-y border-sky-500/40 bg-sky-500/10 px-3 py-1.5 font-mono text-xs text-sky-200"
          >
            {{ draft.priority }} · {{ draft.name || "this rule" }} ← here
          </div>
        </div>
      </div>
    </section>

    <!-- 6. Review -->
    <section v-else-if="step === 'review'" class="space-y-4">
      <div class="rounded-lg border border-slate-800 bg-slate-950/40 p-4 text-sm leading-relaxed text-slate-300">
        <p>
          <b class="font-mono text-slate-100">{{ draft.name || "(unnamed)" }}</b>
          <span class="text-slate-500"> (priority {{ draft.priority }}{{ draft.enabled ? "" : ", disabled" }})</span>
        </p>
        <p class="mt-1"><span class="text-slate-500">When</span> {{ describe(draft.when) }}</p>
        <p v-if="draft.unless"><span class="text-slate-500">unless</span> {{ describe(draft.unless) }}</p>
        <ul class="mt-1 list-inside list-disc text-slate-400">
          <li v-if="draft.profile">boot <code class="text-sky-300">{{ draft.profile }}</code></li>
          <li v-if="draft.tag.length">add tags <code class="text-emerald-300">{{ draft.tag.join(", ") }}</code></li>
          <li v-if="draft.remove_tags.length">remove tags <code class="text-rose-300">{{ draft.remove_tags.join(", ") }}</code></li>
          <li v-for="v in draft.set.filter((v) => v.key)" :key="v.key">set <code>{{ v.key }} = {{ v.value }}</code></li>
          <li>then {{ (draft.stop ?? stopsByDefault) ? "stop" : "carry on" }}</li>
        </ul>
        <p v-if="check.focus" class="mt-2 text-xs text-slate-500">
          Runs {{ check.focus.position + 1 }} of {{ check.focus.of }}<template v-if="check.focus.after">, after
          <code>{{ check.focus.after }}</code></template><template v-if="check.focus.before">, before <code>{{ check.focus.before }}</code></template>.
        </p>
      </div>

      <div>
        <div class="mb-2 flex items-center gap-3">
          <h3 class="text-xs font-semibold uppercase tracking-wider text-slate-400">Effect on known machines</h3>
          <button class="text-xs text-sky-300 hover:text-sky-200" :disabled="previewing" @click="runPreview">
            {{ previewing ? "checking…" : "refresh" }}
          </button>
        </div>
        <div v-if="impact && impact.ok" class="rounded-lg border border-slate-800">
          <p class="px-3 py-2 text-xs text-slate-400">
            Of {{ impact.hosts }} machine{{ impact.hosts === 1 ? "" : "s" }} in the inventory, this rule fires for
            <b class="text-slate-100">{{ impact.fires }}</b>, and
            <b :class="impact.changes ? 'text-amber-300' : 'text-slate-100'">{{ impact.changes }}</b>
            would boot something different.
          </p>
          <table v-if="impact.rows.length" class="w-full text-xs">
            <tr v-for="row in impact.rows" :key="row.mac" class="border-t border-slate-800/70">
              <td class="px-3 py-1 font-mono text-slate-300">{{ row.mac }}</td>
              <td class="px-3 py-1 text-slate-500">{{ row.hostname || row.product || row.vendor || "" }}</td>
              <td class="px-3 py-1 font-mono">
                <span class="text-slate-500">{{ row.before || "nothing" }}</span>
                <template v-if="row.changed"> → <span class="text-amber-300">{{ row.after || "nothing" }}</span></template>
              </td>
              <td class="px-3 py-1 text-[11px] text-slate-600">
                <span v-if="row.overridden" class="text-violet-300/80">pinned — rules do not decide it</span>
                <span v-else-if="!row.fires">changes because of order</span>
              </td>
            </tr>
          </table>
        </div>
        <p v-else-if="impact && !impact.ok" class="text-xs text-rose-300">Fix the problems below to see the effect.</p>
      </div>
    </section>

    <!-- Problems, on every step -->
    <div
      v-if="check.ok === false && check.problems.length && step !== 'start'"
      class="mt-4 rounded-lg border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-xs text-rose-200"
    >
      <div v-for="problem in check.problems" :key="problem">{{ problem }}</div>
    </div>

    <template #actions>
      <button
        v-if="step !== 'start'"
        class="mr-auto rounded-lg px-3 py-1.5 text-sm text-slate-400 hover:text-slate-200 disabled:opacity-40"
        :disabled="savingTemplate"
        @click="saveAsTemplate"
      >
        Save as starter…
      </button>
      <button class="rounded-lg px-3 py-1.5 text-sm text-slate-400 hover:text-slate-200" @click="emit('close')">Cancel</button>
      <button
        v-if="stepIndex > (isNew ? 0 : 1)"
        class="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-300 hover:border-slate-500"
        @click="back"
      >
        Back
      </button>
      <button
        v-if="step !== 'review'"
        class="rounded-lg border border-sky-500/50 px-3 py-1.5 text-sm text-sky-200 hover:bg-sky-500/10"
        @click="next"
      >
        Next
      </button>
      <button
        class="rounded-lg bg-sky-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-sky-300 disabled:opacity-40"
        :disabled="!canSave"
        :title="check.ok ? '' : 'Fix the problems first'"
        @click="emit('save', payload)"
      >
        {{ saving ? "Saving…" : isNew ? "Add the rule" : "Save" }}
      </button>
    </template>
  </Modal>
</template>
