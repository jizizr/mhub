<script setup>
defineProps({
  title: { type: String, default: "" },
  active: { type: Boolean, default: false },
  disabled: { type: Boolean, default: false },
  size: { type: String, default: "md" },
});

defineEmits(["click"]);
</script>

<template>
  <button
    class="icon-button"
    :class="[`icon-button--${size}`, { 'is-active': active }]"
    :title="title"
    :aria-label="title"
    :disabled="disabled"
    @click="$emit('click', $event)"
  >
    <slot />
  </button>
</template>
