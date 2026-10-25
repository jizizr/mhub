<script setup>
import { computed } from "vue";
import { CircleUserRound } from "lucide-vue-next";
import { useAccountStore } from "../../stores/account";

defineProps({ compact: { type: Boolean, default: false } });
const account = useAccountStore();

const label = computed(() => {
  if (account.mode === "guest") return "登录以同步";
  if (account.mode === "expired") return "登录已过期";
  return account.user?.username || "账号";
});
const detail = computed(() => {
  if (account.mode === "guest") return "歌单与播放进度跨设备同步";
  if (account.mode === "expired") return "修改已保存在本设备";
  return ({ synced: "已同步", syncing: "同步中…", loading: "正在连接…", offline: "离线 · 恢复后自动同步", error: account.syncMessage || "同步出错" })[account.sync] || "已登录";
});
const tone = computed(() => account.mode === "expired" || account.sync === "error" ? "warn"
  : account.mode === "linked" && account.sync === "synced" ? "ok" : account.mode === "linked" ? "busy" : "");
</script>

<template>
  <button v-if="account.enabled || account.mode !== 'guest'" class="account-button" :class="{ 'account-button--compact': compact }"
    type="button" :title="`${label} · ${detail}`" :aria-label="`${label}，${detail}`" @click="account.openDialog()">
    <span class="account-button__avatar" :class="tone && `is-${tone}`">
      <template v-if="account.user">{{ account.user.username.slice(0, 1).toUpperCase() }}</template>
      <CircleUserRound v-else :size="compact ? 18 : 20" />
    </span>
    <span v-if="!compact" class="account-button__text">
      <strong>{{ label }}</strong>
      <small>{{ detail }}</small>
    </span>
  </button>
</template>

<style scoped>
.account-button { display: flex; align-items: center; gap: 10px; width: 100%; min-width: 0; padding: 8px 10px; border: 1px solid var(--line); border-radius: 13px; background: var(--surface); color: var(--text); text-align: left; cursor: pointer; }
.account-button:hover { background: var(--surface-hover); }
.account-button__avatar { position: relative; display: grid; flex: 0 0 32px; place-items: center; width: 32px; height: 32px; border-radius: 50%; background: linear-gradient(180deg, #ff6b7f, #e8173f); color: #fff; font-size: 14px; font-weight: 750; }
.account-button__avatar::after { content: ""; position: absolute; right: -1px; bottom: -1px; width: 10px; height: 10px; border: 2px solid var(--bg); border-radius: 50%; background: transparent; }
.account-button__avatar.is-ok::after { background: var(--success); }
.account-button__avatar.is-busy::after { background: #f5a623; }
.account-button__avatar.is-warn::after { background: var(--danger); }
.account-button__text { display: grid; gap: 1px; min-width: 0; }
.account-button__text strong, .account-button__text small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.account-button__text strong { font-size: 13px; }
.account-button__text small { color: var(--text-muted); font-size: 11px; }
.account-button--compact { width: 36px; height: 36px; padding: 0; justify-content: center; border-radius: 50%; }
.account-button--compact .account-button__avatar { flex-basis: 28px; width: 28px; height: 28px; font-size: 12px; }
</style>
