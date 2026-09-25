<script setup>
import { computed, watch } from "vue";
import ConditionNode from "./ConditionNode.vue";
import { describe } from "../lib/schema.js";

/**
 * A whole condition, edited as a tree of boxes, with the sentence it amounts
 * to underneath.
 *
 * The root is always a group, so there is always somewhere to add the next
 * condition. A stored condition that is a single test or a `not` is wrapped
 * in an `all` on the way in; the server treats the two identically.
 */
const props = defineProps({
  modelValue: { type: Object, default: () => ({ all: [] }) },
  /** Words for the empty state, e.g. "No exceptions". */
  emptyHint: { type: String, default: "" },
});
const emit = defineEmits(["update:modelValue"]);

watch(
  () => props.modelValue,
  (value) => {
    if (!value || !("all" in value || "any" in value)) {
      emit("update:modelValue", { all: value ? [value] : [] });
    }
  },
  { immediate: true },
);

const sentence = computed(() => describe(props.modelValue));
</script>

<template>
  <div class="space-y-2">
    <ConditionNode
      v-if="modelValue && ('all' in modelValue || 'any' in modelValue)"
      :node="modelValue"
      root
      @replace="(next) => emit('update:modelValue', next)"
    />
    <p class="rounded-md bg-slate-950/60 px-3 py-2 text-xs text-slate-400">
      <span class="text-slate-600">Reads as:</span>
      {{ modelValue?.all?.length === 0 && emptyHint ? emptyHint : sentence }}
    </p>
  </div>
</template>
