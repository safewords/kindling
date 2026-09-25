<script setup>
import { computed } from "vue";
import ValueInput from "./ValueInput.vue";
import { defaultValue, factGroups, factSpec, newTest, opSpec, valueKind } from "../lib/schema.js";

/**
 * One node of a condition tree: a group (`all` / `any`), a `not`, or a test.
 *
 * Recursive, because the tree is. A node edits its own fields in place and
 * asks its parent to `replace` or `remove` it when the change is to what kind
 * of node it is — wrapping a test in a `not`, turning a group into a test.
 */
defineOptions({ name: "ConditionNode" });

const props = defineProps({
  node: { type: Object, required: true },
  depth: { type: Number, default: 0 },
  root: { type: Boolean, default: false },
});
const emit = defineEmits(["replace", "remove"]);

const kind = computed(() =>
  "all" in props.node ? "all" : "any" in props.node ? "any" : "not" in props.node ? "not" : "test",
);
const children = computed(() => props.node[kind.value]);
const spec = computed(() => (kind.value === "test" ? factSpec(props.node.fact) : null));

// --- groups ---
function setMode(mode) {
  if (mode === kind.value) return;
  emit("replace", { [mode]: children.value });
}
const add = (child) => children.value.push(child);
const replaceChild = (index, next) => children.value.splice(index, 1, next);
const removeChild = (index) => children.value.splice(index, 1);

// --- tests ---
function setFact(fact) {
  const next = factSpec(fact);
  const node = props.node;
  node.fact = fact;
  if (!next.operators.includes(node.op)) node.op = next.operators[0];
  node.value = defaultValue(fact, node.op);
  if (next.keyed) node.key = node.key || "";
  else delete node.key;
}

function setOp(op) {
  const before = valueKind(props.node.fact, props.node.op);
  props.node.op = op;
  const after = valueKind(props.node.fact, op);
  // Keep what was typed when the shape of the value is the same.
  if (before !== after) {
    if (before === "list" && after === "text") props.node.value = props.node.value?.[0] || "";
    else if (before === "text" && after === "list") props.node.value = props.node.value ? [props.node.value] : [];
    else props.node.value = defaultValue(props.node.fact, op);
  }
  if (!opSpec(op).takes_value) delete props.node.value;
}

const negate = () => emit("replace", { not: props.node });
const unwrap = () => emit("replace", props.node.not);

const tint = computed(() =>
  ["border-sky-500/30", "border-violet-500/30", "border-emerald-500/30", "border-amber-500/30"][props.depth % 4],
);
</script>

<template>
  <!-- A group -->
  <div
    v-if="kind === 'all' || kind === 'any'"
    class="rounded-lg border bg-slate-950/40 p-2.5"
    :class="tint"
  >
    <div class="mb-2 flex flex-wrap items-center gap-2 text-xs">
      <div class="flex overflow-hidden rounded border border-slate-700">
        <button
          v-for="mode in ['all', 'any']"
          :key="mode"
          class="px-2 py-0.5"
          :class="kind === mode ? 'bg-slate-700 text-slate-100' : 'text-slate-400 hover:bg-slate-800'"
          @click="setMode(mode)"
        >
          {{ mode === "all" ? "all of" : "any of" }}
        </button>
      </div>
      <span class="text-slate-500">
        {{ kind === "all" ? "every condition below must hold" : "at least one condition below must hold" }}
      </span>
      <div class="ml-auto flex gap-1">
        <button class="rounded px-1.5 py-0.5 text-slate-400 hover:bg-slate-800 hover:text-slate-100" @click="add(newTest())">
          + condition
        </button>
        <button
          class="rounded px-1.5 py-0.5 text-slate-400 hover:bg-slate-800 hover:text-slate-100"
          @click="add({ [kind === 'all' ? 'any' : 'all']: [newTest()] })"
        >
          + group
        </button>
        <button
          v-if="!root"
          class="rounded px-1.5 py-0.5 text-slate-500 hover:text-amber-300"
          title="Negate this group"
          @click="negate"
        >
          not
        </button>
        <button v-if="!root" class="rounded px-1.5 py-0.5 text-slate-600 hover:text-rose-400" @click="emit('remove')">
          ✕
        </button>
      </div>
    </div>

    <p v-if="!children.length" class="px-1 py-1 text-xs" :class="kind === 'all' ? 'text-amber-300/80' : 'text-rose-300/80'">
      {{
        kind === "all"
          ? "No conditions: this matches every machine."
          : "An empty “any” can never hold — add a condition or remove the group."
      }}
    </p>

    <div class="space-y-1.5">
      <template v-for="(child, index) in children" :key="index">
        <div v-if="index > 0" class="pl-2 text-[10px] font-semibold uppercase tracking-wider text-slate-600">
          {{ kind === "all" ? "and" : "or" }}
        </div>
        <ConditionNode
          :node="child"
          :depth="depth + 1"
          @replace="(next) => replaceChild(index, next)"
          @remove="removeChild(index)"
        />
      </template>
    </div>
  </div>

  <!-- A negation -->
  <div v-else-if="kind === 'not'" class="flex items-start gap-2">
    <button
      class="mt-1.5 shrink-0 rounded border border-amber-500/40 bg-amber-500/10 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-amber-300 hover:bg-amber-500/20"
      title="Remove the negation"
      @click="unwrap"
    >
      not
    </button>
    <div class="min-w-0 flex-1">
      <ConditionNode :node="node.not" :depth="depth" @replace="(next) => (node.not = next)" @remove="emit('remove')" />
    </div>
  </div>

  <!-- A test -->
  <div v-else class="rounded-md border border-slate-800 bg-slate-900/60 px-2 py-1.5">
    <div class="flex flex-wrap items-center gap-1.5">
      <select
        :value="node.fact"
        class="rounded border border-slate-700 bg-slate-950 px-1.5 py-1 text-xs text-slate-200 outline-none"
        @change="setFact($event.target.value)"
      >
        <optgroup v-for="group in factGroups()" :key="group.name" :label="group.name">
          <option v-for="fact in group.facts" :key="fact.name" :value="fact.name">{{ fact.label }}</option>
        </optgroup>
      </select>

      <input
        v-if="spec?.keyed"
        v-model="node.key"
        placeholder="name"
        class="w-24 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs text-slate-200 outline-none"
      />

      <select
        :value="node.op"
        class="rounded border border-slate-700 bg-slate-950 px-1.5 py-1 text-xs text-slate-300 outline-none"
        @change="setOp($event.target.value)"
      >
        <option v-for="op in spec?.operators || []" :key="op" :value="op">{{ opSpec(op).label }}</option>
      </select>

      <ValueInput v-model="node.value" :fact="node.fact" :op="node.op" />

      <div class="ml-auto flex shrink-0 gap-0.5">
        <button class="rounded px-1.5 text-xs text-slate-500 hover:text-amber-300" title="Negate" @click="negate">not</button>
        <button class="rounded px-1.5 text-xs text-slate-600 hover:text-rose-400" title="Remove" @click="emit('remove')">✕</button>
      </div>
    </div>
    <p v-if="spec" class="mt-1 text-[11px] leading-snug text-slate-600">
      <span
        v-if="spec.known_at !== 'always'"
        class="mr-1 rounded bg-amber-500/10 px-1 text-amber-300/80"
        :title="`Only known at the ${spec.known_at} stage`"
      >
        {{ spec.known_at }} only
      </span>
      {{ spec.help }}
      <span v-if="opSpec(node.op).value" class="text-slate-700"> — takes {{ opSpec(node.op).value }}.</span>
    </p>
  </div>
</template>
