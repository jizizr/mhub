<script>
import { parseShareParams, stripShareParams } from "../../utils/share";

// Read once per page load: the parameters are removed from the address right
// away, so a reload or a later visit to the home page does not offer the
// shared song again.
let incoming = null;
if (typeof window !== "undefined") {
  incoming = parseShareParams(window.location.search);
  if (incoming) {
    try {
      window.history.replaceState(window.history.state, "", stripShareParams(window.location.href));
    } catch {
      // Keeping the parameters is harmless.
    }
  }
}

export function takeIncomingShare() {
  return incoming;
}
</script>

<script setup>
import { computed, ref } from "vue";
import { Link2, ListStart, Play, X } from "lucide-vue-next";

const emit = defineEmits(["play", "next"]);
const shared = ref(takeIncomingShare());
const coverBroken = ref(false);

const track = computed(() => shared.value && {
  platform: shared.value.platform,
  source_id: shared.value.id,
  selected_id: shared.value.id,
  title: shared.value.title || "分享的歌曲",
  artist: shared.value.artist || "",
  cover: shared.value.cover,
});

function dismiss() {
  shared.value = null;
  incoming = null;
}

function play() {
  emit("play", track.value);
  dismiss();
}

function next() {
  emit("next", track.value);
  dismiss();
}
</script>

<template>
  <Transition name="list-state">
    <div v-if="track" class="shared-track" role="region" aria-label="分享给你的歌曲">
      <span class="shared-track__cover">
        <img v-if="track.cover && !coverBroken" :src="track.cover" alt="" @error="coverBroken = true" />
        <Link2 v-else :size="18" aria-hidden="true" />
      </span>
      <span class="shared-track__text">
        <small>有人分享了一首歌给你</small>
        <strong>{{ track.title }}<span v-if="track.artist"> — {{ track.artist }}</span></strong>
      </span>
      <span class="shared-track__actions">
        <button class="primary-action shared-track__play" type="button" @click="play"><Play :size="15" />播放</button>
        <button class="mini-button" type="button" title="下一首播放" aria-label="下一首播放" @click="next"><ListStart :size="17" /></button>
        <button class="mini-button" type="button" title="忽略" aria-label="忽略" @click="dismiss"><X :size="17" /></button>
      </span>
    </div>
  </Transition>
</template>

<style scoped>
.shared-track { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center; gap: 12px; margin-bottom: 18px; padding: 6px 2px 12px; border-bottom: .5px solid var(--am-divider, var(--line)); }
.shared-track__cover { display: grid; place-items: center; width: 44px; height: 44px; overflow: hidden; border-radius: 7px; background: var(--surface-muted); color: var(--accent); }
.shared-track__cover img { width: 100%; height: 100%; object-fit: cover; display: block; }
.shared-track__text { display: grid; gap: 1px; min-width: 0; }
.shared-track__text small { color: var(--am-secondary, var(--text-muted)); font-size: 12px; }
.shared-track__text strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--am-primary, var(--text)); font-size: 13px; font-weight: 600; }
.shared-track__text strong span { color: var(--am-secondary, var(--text-muted)); font-weight: 400; }
.shared-track__actions { display: flex; align-items: center; gap: 2px; }
.shared-track__play { min-height: 28px; margin-right: 4px; padding: 0 14px; border-radius: 999px; font-size: 13px; font-weight: 600; }
</style>
