<script setup>
import { computed, onBeforeUnmount, onMounted, ref, useId } from "vue";
import { ChevronDown, SlidersHorizontal, X } from "lucide-vue-next";
import * as player from "../../core/player";
import { useStatusStore } from "../../stores/status";

defineProps({
  compact: { type: Boolean, default: false },
  /** A labelled pill for toolbars; its panel floats below like a menu. */
  pill: { type: Boolean, default: false },
  /** Compact panels open above the player bar, or below a page header. */
  placement: { type: String, default: "above" },
});

const status = useStatusStore();
const root = ref(null);
const trigger = ref(null);
const open = ref(false);
const id = useId();
const QUALITY_LABELS = { standard: "标准", high: "高品质", lossless: "无损", hires: "Hi-Res" };
const qualityLabel = computed(() => QUALITY_LABELS[status.quality] || status.quality);
const summary = computed(() => {
  const enabled = [QUALITY_LABELS[status.quality] || status.quality];
  if (status.crossfadeEnabled) enabled.push(`渐入渐出 ${status.crossfadeSeconds} 秒`);
  if (status.soundCheckEnabled) enabled.push("音量平衡");
  return enabled.join(" · ");
});
const balanceMessage = computed(() => {
  if (!status.soundCheckEnabled) return "减小不同歌曲之间的音量差异";
  if (status.soundCheckState === "ready") return "这首歌的音量分析已就绪";
  if (status.soundCheckState === "pending") return "正在分析 · 当前播放不受影响";
  if (status.soundCheckState === "unavailable") return "这首歌暂无法分析，保持原始音量";
  return "分析完成后，将在后续播放应用";
});

function close(restoreFocus = false) {
  if (!open.value) return;
  open.value = false;
  if (restoreFocus) trigger.value?.focus();
}

function onOutsidePointer(event) {
  if (open.value && !root.value?.contains(event.target)) close();
}

function onKeyDown(event) {
  if (open.value && event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    close(true);
  }
}

function changeQuality(event) {
  status.quality = event.target.value;
  player.prepareAudio(event.target.value);
}

function changeRoman(event) {
  status.lyricRoman = event.target.value === "on";
  status.persist();
}

function changePreference(key, event) {
  status[key] = event.target.value;
  status.persist();
}

function changeCrossfade(event) {
  player.prepareAudio();
  player.setCrossfadeEnabled(event.target.value === "crossfade");
}

function changeDuration(event) {
  player.prepareAudio();
  player.setCrossfadeSeconds(Number(event.target.value));
}

function changeBalance(event) {
  player.prepareAudio();
  player.setSoundCheckEnabled(event.target.checked);
}

onMounted(() => {
  document.addEventListener("pointerdown", onOutsidePointer, true);
  document.addEventListener("keydown", onKeyDown, true);
});
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onOutsidePointer, true);
  document.removeEventListener("keydown", onKeyDown, true);
});
</script>

<template>
  <div ref="root" class="listening-settings" :class="{ 'listening-settings--compact': compact, 'listening-settings--pill': pill, 'listening-settings--below': placement === 'below' || pill, 'is-open': open }">
    <button
      :id="`${id}-trigger`"
      ref="trigger"
      class="listening-settings__trigger"
      type="button"
      :title="`设置 · ${summary}`"
      aria-label="设置"
      :aria-expanded="open"
      :aria-controls="`${id}-panel`"
      @click="open = !open"
    >
      <SlidersHorizontal :size="18" aria-hidden="true" />
      <span v-if="pill" class="listening-settings__pill-label">播放设置<small>{{ qualityLabel }}</small></span>
      <span v-else-if="!compact" class="listening-settings__intro">
        <strong>播放设置</strong>
        <small>{{ summary }}</small>
      </span>
      <ChevronDown v-if="!compact && !pill" :size="15" class="listening-settings__chevron" aria-hidden="true" />
      <i v-if="(compact || pill) && (status.crossfadeEnabled || status.soundCheckEnabled)" class="listening-settings__indicator" aria-hidden="true" />
    </button>

    <Transition name="listening-settings">
      <section v-if="open" :id="`${id}-panel`" class="listening-settings__panel" role="region" :aria-labelledby="`${id}-trigger`">
        <div class="listening-settings__heading">
          <strong>设置</strong>
          <button type="button" class="listening-settings__close" aria-label="关闭设置" @click="close(true)"><X :size="17" /></button>
        </div>
        <div class="listening-settings__section">
          <div class="listening-settings__row">
            <label :for="`${id}-quality`">音质</label>
            <select :id="`${id}-quality`" title="音质" :value="status.quality" @change="changeQuality">
              <option value="standard">标准</option>
              <option value="high">高品质</option>
              <option value="lossless">无损</option>
              <option value="hires">Hi-Res</option>
            </select>
          </div>
          <div class="listening-settings__row">
            <label :for="`${id}-lyrics`">歌词</label>
            <select :id="`${id}-lyrics`" title="歌词文字" :value="status.lyricScript" @change="changePreference('lyricScript', $event)">
              <option value="original">原文</option>
              <option value="simplified">简体</option>
              <option value="traditional">繁體</option>
            </select>
          </div>
          <div class="listening-settings__row">
            <label :for="`${id}-lyric-words`">逐字</label>
            <select :id="`${id}-lyric-words`" title="逐字歌词" :value="status.lyricWordMode" @change="changePreference('lyricWordMode', $event)">
              <option value="native">原生逐字</option>
              <option value="estimate">无逐字时匀速估算</option>
              <option value="line">逐行</option>
            </select>
          </div>
          <div class="listening-settings__row">
            <label :for="`${id}-lyric-roman`">音译</label>
            <select :id="`${id}-lyric-roman`" title="歌词音译" :value="status.lyricRoman ? 'on' : 'off'" @change="changeRoman">
              <option value="off">隐藏</option>
              <option value="on">显示</option>
            </select>
          </div>
          <div class="listening-settings__row">
            <label :for="`${id}-theme`">外观</label>
            <select :id="`${id}-theme`" title="外观" :value="status.theme" @change="changePreference('theme', $event)">
              <option value="system">跟随系统</option>
              <option value="light">浅色</option>
              <option value="dark">深色</option>
            </select>
          </div>
        </div>
        <div class="listening-settings__section">
          <div class="listening-settings__row">
            <label :for="`${id}-transition`">歌曲过渡</label>
            <select :id="`${id}-transition`" :value="status.crossfadeEnabled ? 'crossfade' : 'off'" @change="changeCrossfade">
              <option value="off">关闭</option>
              <option value="crossfade">交叉渐入渐出</option>
            </select>
          </div>
          <div v-if="status.crossfadeEnabled" class="listening-settings__duration">
            <label :for="`${id}-duration`">过渡时长</label>
            <output :for="`${id}-duration`">{{ status.crossfadeSeconds }} 秒</output>
            <input :id="`${id}-duration`" type="range" min="1" max="12" step="1" :value="status.crossfadeSeconds" :aria-valuetext="`${status.crossfadeSeconds} 秒`" @input="changeDuration" />
          </div>
          <p>下一首准备好时，在歌曲末尾平滑衔接。</p>
          <p v-if="status.crossfadeEnabled && status.transitionState === 'mixing'" class="listening-settings__live" role="status">正在衔接下一首</p>
        </div>
        <div class="listening-settings__section">
          <label class="listening-settings__balance" :for="`${id}-balance`">
            <span>音量平衡</span>
            <input :id="`${id}-balance`" type="checkbox" :checked="status.soundCheckEnabled" @change="changeBalance" />
          </label>
          <p role="status" aria-live="polite">{{ balanceMessage }}</p>
          <p class="listening-settings__note">整曲分析后，在后续播放应用。首次播放不等待分析。</p>
        </div>
        <p class="listening-settings__links">
          <a class="listening-settings__source" href="/source.tar.gz">MHub 源代码 · AGPL-3.0</a>
          <a class="listening-settings__source" href="/docs/api/" target="_blank" rel="noopener">API 文档</a>
        </p>
      </section>
    </Transition>
  </div>
</template>

<style scoped>
.listening-settings { position: relative; flex: 0 0 auto; min-width: 0; margin-bottom: 12px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface); color: var(--text); }
.listening-settings__trigger { position: relative; display: flex; align-items: center; gap: 10px; width: 100%; min-height: 48px; padding: 10px 12px; border: 0; border-radius: 13px; color: inherit; background: transparent; text-align: left; cursor: pointer; }
.listening-settings__trigger:hover { background: var(--surface-muted); }
.listening-settings__trigger > svg { flex: 0 0 auto; }
.listening-settings__intro { display: grid; gap: 3px; min-width: 0; }
.listening-settings__intro strong { font-size: 13px; font-weight: 650; }
.listening-settings__intro small { color: var(--text-muted); font-size: 11px; line-height: 1.4; }
.listening-settings__chevron { margin-left: auto; transition: transform var(--motion-fast); }
.is-open .listening-settings__chevron { transform: rotate(180deg); }
.listening-settings__panel { max-height: min(360px, 45dvh); padding: 0 12px 10px; overflow: auto; overscroll-behavior: contain; text-align: left; }
.listening-settings__heading { display: none; align-items: center; justify-content: space-between; margin-bottom: 10px; font-size: 15px; }
.listening-settings__close { display: grid; place-items: center; width: 32px; height: 32px; padding: 0; border: 0; border-radius: 50%; background: var(--surface-muted); color: var(--text-muted); cursor: pointer; }
.listening-settings__section { padding: 12px 0 2px; border-top: 1px solid var(--line); }
.listening-settings__section + .listening-settings__section { margin-top: 12px; }
.listening-settings__row + .listening-settings__row { margin-top: 10px; }
.listening-settings__links { display: flex; flex-wrap: wrap; gap: 6px 16px; margin: 14px 0 0; }
.listening-settings__source { color: var(--text-muted); font-size: 12px; text-decoration: none; }
.listening-settings__source:hover { color: var(--accent); }
.listening-settings__row, .listening-settings__balance { display: flex; align-items: center; justify-content: space-between; gap: 12px; font-size: 13px; font-weight: 650; }
.listening-settings__row label { flex: 0 0 auto; }
.listening-settings__row select { min-width: 0; max-width: 65%; padding: 7px 8px; border: 1px solid var(--line); border-radius: 8px; color: var(--text); background: var(--surface); font: inherit; font-weight: 400; }
/* The select's translucent fill would tint the native option popup almost
   white under its light text; paint options in the opaque system colours of
   the active color-scheme instead (dark inside the full player). */
.listening-settings__row option { color: CanvasText; background-color: Canvas; }
.listening-settings__section p { margin: 8px 0 0; color: var(--text-muted); font-size: 12px; line-height: 1.6; }
.listening-settings__section .listening-settings__note { font-size: 11px; }
.listening-settings__section .listening-settings__live { color: var(--accent); }
.listening-settings__duration { display: grid; grid-template-columns: 1fr auto; align-items: center; gap: 6px 12px; margin-top: 14px; color: var(--text-muted); font-size: 12px; }
.listening-settings__duration output { color: var(--text); font-variant-numeric: tabular-nums; }
.listening-settings__duration input { grid-column: 1 / -1; width: 100%; margin: 0; accent-color: var(--accent); }
.listening-settings__balance { cursor: pointer; }
.listening-settings__balance input { width: 18px; height: 18px; margin: 0; accent-color: var(--accent); cursor: pointer; }
.listening-settings button:focus-visible, .listening-settings select:focus-visible, .listening-settings input:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
.listening-settings--compact { margin: 0; border: 0; border-radius: 50%; background: transparent; }
.listening-settings--compact .listening-settings__trigger { justify-content: center; width: 34px; min-height: 34px; padding: 0; border-radius: 50%; color: var(--text-muted); }
.listening-settings__indicator { position: absolute; top: 3px; right: 2px; width: 5px; height: 5px; border-radius: 50%; background: var(--accent); }
.listening-settings--compact .listening-settings__panel { position: absolute; z-index: 1; bottom: calc(50% + var(--player-bar-height) * .5 + 12px); right: 0; width: min(320px, calc(100vw - 32px)); max-height: calc(100dvh - var(--player-dock-height) - 48px); padding: 14px 16px 16px; border: 1px solid var(--line); border-radius: 18px; background: var(--surface-raised); box-shadow: var(--shadow); }
.listening-settings--compact .listening-settings__heading { display: flex; }
.listening-settings--below .listening-settings__panel { top: calc(100% + 8px); bottom: auto; max-height: min(560px, calc(100dvh - 120px)); background: var(--bar-solid, var(--surface-raised)); box-shadow: var(--shadow-lg); }
.listening-settings--below .listening-settings__trigger { width: 38px; min-height: 38px; color: var(--text); background: var(--surface-muted); }
.listening-settings--pill { margin: 0; border: 0; border-radius: 999px; background: transparent; }
.listening-settings--pill .listening-settings__trigger { gap: 6px; width: auto; min-height: 30px; padding: 0 10px; border-radius: 999px; color: var(--am-secondary, var(--text-muted)); background: transparent; font-size: 13px; font-weight: 600; }
.listening-settings--pill .listening-settings__trigger:hover, .listening-settings--pill.is-open .listening-settings__trigger { background: var(--nav-selected, var(--surface-muted)); color: var(--am-primary, var(--text)); }
.listening-settings__pill-label { display: flex; align-items: baseline; gap: 5px; white-space: nowrap; }
.listening-settings__pill-label small { color: var(--am-tertiary, var(--text-muted)); font-size: 12px; font-weight: 500; }
.listening-settings--pill .listening-settings__indicator { top: 4px; right: 4px; }
.listening-settings--pill .listening-settings__panel { position: absolute; z-index: 5; right: 0; width: min(300px, calc(100vw - 32px)); padding: 14px 16px 16px; border: 1px solid var(--line); border-radius: 18px; color: var(--text); }
.listening-settings--pill .listening-settings__heading { display: flex; }
.listening-settings-enter-active, .listening-settings-leave-active { transition: opacity var(--motion-fast), transform var(--motion-fast) var(--ease-standard); }
.listening-settings-enter-from, .listening-settings-leave-to { opacity: 0; transform: translateY(5px); }
@media (prefers-reduced-motion: reduce) { .listening-settings__chevron, .listening-settings-enter-active, .listening-settings-leave-active { transition: none; } }
</style>
