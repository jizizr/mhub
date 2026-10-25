<script setup>
import { computed } from "vue";
import { MonitorSmartphone } from "lucide-vue-next";
import * as player from "../../core/player";
import { useAccountStore } from "../../stores/account";
import { trackFromSearch } from "../../utils/track";

const props = defineProps({ maxAgeMinutes: { type: Number, default: 12 * 60 } });
const account = useAccountStore();

// The most recent other device that has a queue, if it is recent enough.
const session = computed(() => account.otherDevices.find((item) =>
  Date.now() / 1000 - (item.updated_at || 0) <= props.maxAgeMinutes * 60));
const current = computed(() => session.value?.queue?.[session.value.play_index] || null);
const ago = computed(() => {
  const minutes = Math.max(0, Math.round((Date.now() / 1000 - (session.value?.updated_at || 0)) / 60));
  return minutes < 1 ? "刚刚" : minutes < 60 ? `${minutes} 分钟前` : `${Math.round(minutes / 60)} 小时前`;
});

// Take over the other device's queue and position on this device. The
// quality is this device's current setting, as everywhere else.
async function continueHere() {
  const source = session.value;
  if (!source) return;
  const tracks = source.queue.map((item) => trackFromSearch({
    platform: item.platform, id: item.source_id, song: item.title, singer: item.artist, cover: item.cover,
  }, "high"));
  const started = await player.playFrom(tracks, source.play_index, {
    id: `handoff:${source.device_id}`,
    name: `来自 ${source.device_name}`,
    type: "handoff",
  });
  if (started && source.position_ms > 3000) player.seek(source.position_ms);
}
</script>

<template>
  <Transition name="list-state">
    <div v-if="session && current" class="handoff" role="region" aria-label="在其他设备上播放">
      <MonitorSmartphone :size="18" class="handoff__icon" aria-hidden="true" />
      <span class="handoff__text">
        <small>{{ session.device_name }} · {{ session.is_playing ? "正在播放" : "暂停" }} · {{ ago }}</small>
        <strong>{{ current.title }}<span> — {{ current.artist }}</span></strong>
      </span>
      <button class="primary-action handoff__action" type="button" @click="continueHere">在此设备继续</button>
    </div>
  </Transition>
</template>

<style scoped>
.handoff { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center; gap: 12px; margin-bottom: 14px; padding: 10px 12px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface); }
.handoff__icon { color: var(--accent); }
.handoff__text { display: grid; gap: 2px; min-width: 0; }
.handoff__text small { color: var(--text-muted); font-size: 11px; }
.handoff__text strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 13px; }
.handoff__text strong span { color: var(--text-muted); font-weight: 500; }
.handoff__action { min-height: 32px; padding: 0 12px; font-size: 12px; }
</style>
