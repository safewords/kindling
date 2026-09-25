<script setup>
import { computed, ref, watch } from "vue";

/**
 * A plain-text editor for a document the server validates.
 *
 * Deliberately not a syntax-highlighting editor component. What makes editing
 * a boot policy safe is not colour — it is knowing whether the thing you typed
 * would load, and which line the server objected to. So the effort is in the
 * gutter, the error line and the validation panel beside it, and the bundle
 * stays small enough to ship to a rack room on a bad link.
 */

const props = defineProps({
  modelValue: { type: String, default: "" },
  /** Problems from the server, shown under the editor. */
  problems: { type: Array, default: () => [] },
  ok: { type: Boolean, default: false },
  checking: { type: Boolean, default: false },
  readonly: { type: Boolean, default: false },
  minRows: { type: Number, default: 24 },
});

const emit = defineEmits(["update:modelValue"]);

const area = ref(null);
const gutter = ref(null);

const lines = computed(() => Math.max(props.modelValue.split("\n").length, props.minRows));

/**
 * The line the server complained about, when it named one.
 *
 * TOML parse errors carry `at line 12 column 3`, and pointing at the line is
 * most of the value of showing the message at all.
 */
const blamedLine = computed(() => {
  for (const problem of props.problems) {
    const match = /line (\d+)/i.exec(problem);
    if (match) return Number(match[1]);
  }
  return null;
});

const syncScroll = () => {
  if (gutter.value && area.value) gutter.value.scrollTop = area.value.scrollTop;
};

/**
 * Tab inserts two spaces rather than leaving the field.
 *
 * Losing focus mid-document is the single most irritating thing a textarea
 * does to somebody editing indented TOML, and the accessibility escape hatch
 * is still there: Escape then Tab moves on.
 */
const onTab = (event) => {
  event.preventDefault();
  const el = event.target;
  const { selectionStart: start, selectionEnd: end, value } = el;
  const next = `${value.slice(0, start)}  ${value.slice(end)}`;
  emit("update:modelValue", next);
  requestAnimationFrame(() => {
    el.selectionStart = el.selectionEnd = start + 2;
  });
};

watch(() => props.modelValue, syncScroll);
</script>

<template>
  <div>
    <div
      class="flex overflow-hidden rounded-lg border bg-slate-950"
      :class="problems.length ? 'border-rose-500/40' : 'border-slate-800 focus-within:border-slate-600'"
    >
      <div
        ref="gutter"
        class="doc-editor max-h-[65vh] shrink-0 select-none overflow-hidden border-r border-slate-800/80 bg-slate-900/40 px-3 py-3 text-right font-mono text-xs text-slate-600"
        aria-hidden="true"
      >
        <div
          v-for="n in lines"
          :key="n"
          :class="n === blamedLine ? 'bg-rose-500/20 text-rose-300 -mx-3 px-3' : ''"
        >
          {{ n }}
        </div>
      </div>

      <textarea
        ref="area"
        class="doc-editor max-h-[65vh] min-h-[24rem] w-full resize-y bg-transparent px-3 py-3 font-mono text-[13px] text-slate-200 outline-none"
        :value="modelValue"
        :readonly="readonly"
        spellcheck="false"
        autocapitalize="off"
        autocorrect="off"
        @input="emit('update:modelValue', $event.target.value)"
        @scroll="syncScroll"
        @keydown.tab="onTab"
      ></textarea>
    </div>

    <div class="mt-2 min-h-[1.5rem] text-xs">
      <div v-if="checking" class="text-slate-500">Checking…</div>

      <ul v-else-if="problems.length" class="space-y-1">
        <li
          v-for="(problem, index) in problems"
          :key="index"
          class="flex gap-2 rounded border border-rose-500/25 bg-rose-500/5 px-2.5 py-1.5 text-rose-200"
        >
          <span class="select-none text-rose-400">✕</span>
          <span class="whitespace-pre-wrap">{{ problem }}</span>
        </li>
      </ul>

      <div v-else-if="ok" class="text-emerald-400">
        ✓ This would load.
      </div>
    </div>
  </div>
</template>
