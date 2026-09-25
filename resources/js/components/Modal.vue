<script setup>
import { onMounted, onBeforeUnmount } from "vue";

defineProps({
  title: { type: String, default: "" },
  subtitle: { type: String, default: "" },
  wide: { type: Boolean, default: false },
});

const emit = defineEmits(["close"]);

// Escape closes. A dialog on a server interface that traps somebody with no
// way out but the mouse is a dialog they will resent at 3am.
const onKey = (event) => {
  if (event.key === "Escape") emit("close");
};

onMounted(() => window.addEventListener("keydown", onKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onKey));
</script>

<template>
  <div class="fixed inset-0 z-40 flex items-start justify-center overflow-y-auto bg-slate-950/70 p-4 backdrop-blur-sm">
    <!-- Clicking the backdrop closes; clicking the panel does not. -->
    <div class="absolute inset-0" @click="emit('close')"></div>

    <div
      class="rise relative my-8 w-full rounded-xl border border-slate-700 bg-slate-900 shadow-2xl shadow-black/60"
      :class="wide ? 'max-w-4xl' : 'max-w-xl'"
    >
      <header class="flex items-start gap-4 border-b border-slate-800 px-5 py-4">
        <div class="flex-1">
          <h2 class="text-sm font-semibold text-slate-100">{{ title }}</h2>
          <p v-if="subtitle" class="mt-0.5 text-xs text-slate-500">{{ subtitle }}</p>
        </div>
        <button
          class="-m-1 rounded p-1 text-slate-500 hover:bg-slate-800 hover:text-slate-200"
          aria-label="Close"
          @click="emit('close')"
        >
          ✕
        </button>
      </header>

      <div class="px-5 py-4">
        <slot />
      </div>

      <footer v-if="$slots.actions" class="flex justify-end gap-2 border-t border-slate-800 px-5 py-3">
        <slot name="actions" />
      </footer>
    </div>
  </div>
</template>
