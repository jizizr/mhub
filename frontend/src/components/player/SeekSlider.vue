<script setup>
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { formatTime } from "../../utils/format";

const props = defineProps({
  position: { type: Number, default: 0 },
  duration: { type: Number, default: 0 },
  trackKey: { type: String, default: "" },
  disabled: Boolean,
});
const emit = defineEmits(["preview", "seek"]);
const input = ref(null);
const draft = ref(null);
const value = computed(() => draft.value ?? props.position);
const disabled = computed(() => props.disabled || !(props.duration > 0));
let pointerId = null;
let interaction = null;

function clamp(value) {
  return Math.min(props.duration, Math.max(0, Number.isFinite(value) ? value : 0));
}

function preview(value) {
  draft.value = clamp(value);
  emit("preview", draft.value);
}

function reset() {
  const capturedId = pointerId;
  pointerId = null;
  interaction = null;
  draft.value = null;
  emit("preview", null);
  if (capturedId !== null && input.value?.hasPointerCapture(capturedId)) {
    input.value.releasePointerCapture(capturedId);
  }
}

function commit() {
  if (!interaction || draft.value === null || disabled.value) return reset();
  const target = clamp(draft.value);
  // Only one seek per gesture. In particular, a range's `change` and
  // `pointerup` must not start two competing decoder resets.
  interaction = null;
  emit("seek", target);
  reset();
}

function pointerValue(event) {
  const rect = input.value.getBoundingClientRect();
  const fraction = rect.width > 0 ? (event.clientX - rect.left) / rect.width : 0;
  return clamp(fraction * props.duration);
}

function beginPointer(event) {
  if (disabled.value || event.button !== 0 || event.isPrimary === false) return;
  event.preventDefault();
  input.value.focus({ preventScroll: true });
  pointerId = event.pointerId;
  interaction = "pointer";
  input.value.setPointerCapture(pointerId);
  preview(pointerValue(event));
}

function movePointer(event) {
  if (event.pointerId !== pointerId || interaction !== "pointer") return;
  preview(pointerValue(event));
}

function finishPointer(event) {
  if (event.pointerId !== pointerId || interaction !== "pointer") return;
  preview(pointerValue(event));
  commit();
}

function cancelPointer(event) {
  if (event.pointerId === pointerId) reset();
}

const seekKeys = new Set(["ArrowLeft", "ArrowRight", "ArrowDown", "ArrowUp", "PageDown", "PageUp", "Home", "End"]);
function keydown(event) {
  if (event.key === "Escape" && interaction) {
    event.preventDefault();
    reset();
    return;
  }
  if (disabled.value || !seekKeys.has(event.key) || interaction === "pointer") return;
  event.preventDefault();
  interaction = "keyboard";
  const steps = { ArrowLeft: -5000, ArrowDown: -5000, ArrowRight: 5000, ArrowUp: 5000, PageDown: -10000, PageUp: 10000 };
  preview(event.key === "Home" ? 0 : event.key === "End" ? props.duration : value.value + steps[event.key]);
}

function keyup(event) {
  if (interaction === "keyboard" && seekKeys.has(event.key)) commit();
}

function accessibleInput(event) {
  if (disabled.value || interaction === "pointer") return;
  interaction = "accessible";
  preview(Number(event.target.value));
}

function accessibleChange() {
  if (interaction === "accessible") commit();
}

function blur() {
  if (interaction === "pointer") reset();
  else if (interaction) commit();
}

watch(() => props.trackKey, reset, { flush: "sync" });
watch(disabled, (value) => { if (value) reset(); }, { flush: "sync" });
onBeforeUnmount(reset);
</script>

<template>
  <input
    ref="input"
    class="seek-slider"
    type="range"
    min="0"
    :max="duration || 1"
    step="1"
    :value="value"
    :disabled="disabled"
    aria-label="播放进度"
    :aria-valuetext="`${formatTime(value)} / ${formatTime(duration)}`"
    @pointerdown="beginPointer"
    @pointermove="movePointer"
    @pointerup="finishPointer"
    @pointercancel="cancelPointer"
    @lostpointercapture="cancelPointer"
    @keydown="keydown"
    @keyup="keyup"
    @input="accessibleInput"
    @change="accessibleChange"
    @blur="blur"
  />
</template>

<style scoped>
.seek-slider {
  min-width: 0;
  min-height: 24px;
  margin: 0;
  cursor: pointer;
  touch-action: none;
}
.seek-slider:disabled { cursor: default; }
</style>
