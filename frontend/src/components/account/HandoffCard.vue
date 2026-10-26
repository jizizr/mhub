<script setup>
import { computed } from "vue";
import { MonitorSmartphone } from "lucide-vue-next";
import * as player from "../../core/player";
import { useAccountStore } from "../../stores/account";
import { useStatusStore } from "../../stores/status";
import { trackFromSearch } from "../../utils/track";

const props = defineProps({ maxAgeMinutes: { type: Number, default: 12 * 60 } });
const account = useAccountStore();
const status = useStatusStore();

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
  // keepStart: this device's shuffle setting must not replace the song being
  // continued with a random one.
  const started = await player.playFrom(tracks, source.play_index, {
    id: `handoff:${source.device_id}`,
    name: `来自 ${source.device_name}`,
    type: "handoff",
  }, { keepStart: true });
  const position = estimatedPosition(source);
  if (started && position > 3000) player.seek(position);
}

// A playing device reports every 15 s; continue from where it is now. The cap
// bounds clock skew and a device that stopped reporting.
const MAX_ELAPSED_MS = 20_000;
function estimatedPosition(source) {
  const position = source.position_ms || 0;
  if (!source.is_playing) return position;
  const elapsed = Math.min(MAX_ELAPSED_MS, Math.max(0, Date.now() - (source.updated_at || 0) * 1000));
  const duration = status.duration || 0;
  const estimate = position + elapsed;
  return duration > 0 ? Math.min(estimate, Math.max(position, duration - 5000)) : position;
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
/* A slim row over a hairline, as Apple Music shows device hand-off, instead of a boxed card. */
.handoff { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center; gap: 12px; margin-bottom: 18px; padding: 6px 2px 12px; border-bottom: .5px solid var(--am-divider, var(--line)); }
.handoff__icon { color: var(--accent); }
.handoff__text { display: grid; gap: 1px; min-width: 0; }
.handoff__text small { color: var(--am-secondary, var(--text-muted)); font-size: 12px; }
.handoff__text strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--am-primary, var(--text)); font-size: 13px; font-weight: 600; }
.handoff__text strong span { color: var(--am-secondary, var(--text-muted)); font-weight: 400; }
.handoff__action { min-height: 28px; padding: 0 14px; border-radius: 999px; font-size: 13px; font-weight: 600; }
</style>
