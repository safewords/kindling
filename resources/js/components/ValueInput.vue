<script setup>
import { computed, ref } from "vue";
import { factSpec, valueKind } from "../lib/schema.js";

/**
 * The value box of one test. Which box depends on the fact and the operator —
 * a list of patterns, a number, a yes/no, a window of the day, a set of
 * choices — and all of that comes from the schema, so this file never names
 * a fact.
 */
const props = defineProps({
  fact: { type: String, required: true },
  op: { type: String, required: true },
  modelValue: { default: null },
});
const emit = defineEmits(["update:modelValue"]);

const spec = computed(() => factSpec(props.fact));
const kind = computed(() => valueKind(props.fact, props.op));
const draft = ref("");

const list = computed(() => {
  const v = props.modelValue;
  if (Array.isArray(v)) return v;
  if (v === null || v === undefined || v === "") return [];
  return [v];
});

function set(value) {
  emit("update:modelValue", value);
}

/** Commas and Enter both end an item — people paste lists either way. */
function commit() {
  const items = draft.value
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  if (items.length) set([...list.value, ...items.filter((i) => !list.value.includes(i))]);
  draft.value = "";
}

function onKey(event) {
  if (event.key === "Enter" || event.key === ",") {
    event.preventDefault();
    commit();
  } else if (event.key === "Backspace" && !draft.value && list.value.length) {
    set(list.value.slice(0, -1));
  }
}

const remove = (item) => set(list.value.filter((i) => i !== item));
const toggle = (choice) =>
  set(list.value.includes(choice) ? list.value.filter((c) => c !== choice) : [...list.value, choice]);

const pair = computed(() => (Array.isArray(props.modelValue) ? props.modelValue : ["", ""]));
const setPair = (index, value) => {
  const next = [...pair.value];
  next[index] = value;
  set(next);
};

const box =
  "rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs text-slate-200 outline-none focus:border-sky-500/60";
</script>

<template>
  <span v-if="kind === 'none'" class="text-xs italic text-slate-600">no value needed</span>

  <div
    v-else-if="kind === 'list'"
    class="flex min-w-0 flex-1 flex-wrap items-center gap-1 rounded border border-slate-700 bg-slate-950 px-1.5 py-1 focus-within:border-sky-500/60"
  >
    <span
      v-for="item in list"
      :key="item"
      class="inline-flex items-center gap-1 rounded bg-slate-800 px-1.5 py-0.5 font-mono text-[11px] text-slate-200"
    >
      {{ item }}
      <button class="text-slate-500 hover:text-rose-400" @click="remove(item)">×</button>
    </span>
    <input
      v-model="draft"
      :placeholder="list.length ? 'or…' : spec?.example"
      class="min-w-24 flex-1 bg-transparent px-1 font-mono text-xs text-slate-200 outline-none placeholder:text-slate-600"
      @keydown="onKey"
      @blur="commit"
    />
  </div>

  <input
    v-else-if="kind === 'text'"
    :value="modelValue"
    :placeholder="spec?.example"
    :class="[box, 'min-w-0 flex-1']"
    @input="set($event.target.value)"
  />

  <input
    v-else-if="kind === 'number'"
    type="number"
    :value="modelValue"
    :class="[box, 'w-28']"
    @input="set(Number($event.target.value))"
  />

  <div v-else-if="kind === 'range'" class="flex items-center gap-2">
    <input type="number" :value="pair[0]" :class="[box, 'w-20']" @input="setPair(0, Number($event.target.value))" />
    <span class="text-xs text-slate-500">and</span>
    <input type="number" :value="pair[1]" :class="[box, 'w-20']" @input="setPair(1, Number($event.target.value))" />
  </div>

  <div v-else-if="kind === 'window'" class="flex items-center gap-2">
    <input type="time" :value="pair[0]" :class="box" @input="setPair(0, $event.target.value)" />
    <span class="text-xs text-slate-500">to</span>
    <input type="time" :value="pair[1]" :class="box" @input="setPair(1, $event.target.value)" />
  </div>

  <div v-else-if="kind === 'bool'" class="flex overflow-hidden rounded border border-slate-700 text-xs">
    <button
      v-for="option in [true, false]"
      :key="String(option)"
      class="px-3 py-1"
      :class="modelValue === option ? 'bg-sky-500/20 text-sky-200' : 'text-slate-400 hover:bg-slate-800'"
      @click="set(option)"
    >
      {{ option ? "yes" : "no" }}
    </button>
  </div>

  <select
    v-else-if="kind === 'choice'"
    :value="modelValue"
    :class="box"
    @change="set($event.target.value)"
  >
    <option v-for="choice in spec?.choices || []" :key="choice" :value="choice">{{ choice }}</option>
  </select>

  <div v-else-if="kind === 'choices'" class="flex flex-wrap gap-1">
    <button
      v-for="choice in spec?.choices || []"
      :key="choice"
      class="rounded border px-2 py-0.5 font-mono text-[11px]"
      :class="
        list.includes(choice)
          ? 'border-sky-500/60 bg-sky-500/15 text-sky-200'
          : 'border-slate-700 text-slate-500 hover:border-slate-500 hover:text-slate-300'
      "
      @click="toggle(choice)"
    >
      {{ choice }}
    </button>
  </div>
</template>
