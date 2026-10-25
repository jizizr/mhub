<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { KeyRound, LogOut, ShieldCheck, Smartphone, X } from "lucide-vue-next";
import {
  addPasskey, listSessions, passkeyLogin, passkeysSupported, passwordLogin, recoveryLogin,
  register, revokeSession, setPassword,
} from "../../services/account";
import { useAccountStore } from "../../stores/account";
import { useStatusStore } from "../../stores/status";

const account = useAccountStore();
const status = useStatusStore();
const view = computed({ get: () => account.dialogView, set: (value) => { account.dialogView = value; } });
const supportsPasskey = passkeysSupported();
const busy = ref(false);
const error = ref("");
const username = ref("");
const password = ref("");
const usePassword = ref(!supportsPasskey);
const code = ref("");
const recoveryCodes = ref([]);
const needsPassword = ref(false);
const currentPassword = ref("");
const newPassword = ref("");
const sessions = ref([]);
const clearLocal = ref(true);
const turnstileBox = ref(null);
let turnstileToken = "";
let turnstileWidget = null;

const plan = computed(() => account.pendingMerge);
const titles = { login: "登录 MHub", register: "创建账号", recovery: "使用恢复码登录", codes: "保存恢复码", account: "账号与同步", merge: "合并此设备上的资料" };

async function run(task) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  try {
    await task();
  } catch (reason) {
    // A dismissed passkey prompt is not an error worth showing.
    if (reason?.name !== "NotAllowedError" && reason?.name !== "AbortError") error.value = reason?.message || "操作失败，请重试";
  } finally {
    busy.value = false;
  }
}

async function signedIn(result) {
  await account.completeLogin(result.user);
  if (account.dialogView !== "merge" && !recoveryCodes.value.length) account.dialogOpen = false;
}

const loginWithPasskey = () => run(async () => signedIn(await passkeyLogin()));
const loginWithPassword = () => run(async () => signedIn(await passwordLogin({ username: username.value, password: password.value })));
const loginWithCode = () => run(async () => {
  const result = await recoveryLogin({ username: username.value, code: code.value });
  await account.completeLogin(result.user);
  needsPassword.value = true;
  view.value = "account";
  void loadSessions();
});

const createAccount = () => run(async () => {
  const result = await register({
    username: username.value,
    password: usePassword.value ? password.value : "",
    turnstileToken,
  });
  recoveryCodes.value = result.recovery_codes || [];
  if (!usePassword.value) {
    try {
      await addPasskey();
    } catch (reason) {
      // The account exists; without a passkey it needs a password.
      needsPassword.value = true;
      error.value = reason?.name === "NotAllowedError" ? "通行密钥未创建，请设置一个密码" : (reason?.message || "通行密钥未创建");
    }
  }
  // Merging (if any) waits until the codes are acknowledged.
  await account.completeLogin(result.user);
  view.value = "codes";
});

const createPasskey = () => run(async () => {
  await addPasskey();
  status.toast("已添加通行密钥");
  account.user = { ...account.user, passkeys: (account.user?.passkeys || 0) + 1 };
});

const savePassword = () => run(async () => {
  await setPassword({ password: newPassword.value, currentPassword: currentPassword.value });
  needsPassword.value = false;
  newPassword.value = currentPassword.value = "";
  account.user = { ...account.user, has_password: true };
  status.toast("密码已保存");
});

async function loadSessions() {
  try { sessions.value = (await listSessions()).sessions || []; } catch { sessions.value = []; }
}

const revoke = (session) => run(async () => {
  await revokeSession(session.id);
  sessions.value = sessions.value.filter((item) => item.id !== session.id);
});

const signOut = () => run(() => account.logout({ clearLocal: clearLocal.value }));

function copyCodes() {
  void navigator.clipboard?.writeText(recoveryCodes.value.join("\n")).then(() => status.toast("恢复码已复制"));
}

function finishCodes() {
  recoveryCodes.value = [];
  if (account.pendingMerge) view.value = "merge";
  else if (needsPassword.value) view.value = "account";
  else account.dialogOpen = false;
}

function close() {
  if (view.value === "codes") return; // Codes must be acknowledged once.
  if (view.value === "merge") { account.resolveMerge(false); return; }
  account.dialogOpen = false;
}

function relative(seconds) {
  const minutes = Math.max(0, Math.round((Date.now() / 1000 - seconds) / 60));
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;
  if (minutes < 1440) return `${Math.round(minutes / 60)} 小时前`;
  return `${Math.round(minutes / 1440)} 天前`;
}

function renderTurnstile() {
  if (!account.turnstileSiteKey || !turnstileBox.value) return;
  const draw = () => {
    if (turnstileWidget !== null || !turnstileBox.value) return;
    turnstileWidget = window.turnstile.render(turnstileBox.value, {
      sitekey: account.turnstileSiteKey,
      callback: (token) => { turnstileToken = token; },
      "expired-callback": () => { turnstileToken = ""; },
    });
  };
  if (window.turnstile) { draw(); return; }
  const script = document.createElement("script");
  script.src = "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit";
  script.async = true;
  script.onload = draw;
  document.head.appendChild(script);
}

watch(view, async (value) => {
  error.value = "";
  if (value === "register") { await nextTick(); renderTurnstile(); }
  if (value === "account") void loadSessions();
}, { immediate: true });

function keydown(event) { if (event.key === "Escape") close(); }
onMounted(() => document.addEventListener("keydown", keydown));
onBeforeUnmount(() => {
  document.removeEventListener("keydown", keydown);
  if (turnstileWidget !== null) window.turnstile?.remove(turnstileWidget);
});
</script>

<template>
  <div class="dialog-backdrop" @click.self="close">
    <section class="dialog account" role="dialog" aria-modal="true" aria-labelledby="account-title">
      <header class="account__head">
        <h2 id="account-title">{{ titles[view] }}</h2>
        <button v-if="view !== 'codes'" class="mini-button" type="button" aria-label="关闭" @click="close"><X :size="18" /></button>
      </header>

      <!-- Sign in -->
      <div v-if="view === 'login'" class="account__body">
        <p class="account__lead">登录后，歌单、喜欢和播放进度会在你的所有设备间同步。</p>
        <p v-if="account.mode === 'expired'" class="account__note">登录已过期。本设备的修改都已保存，重新登录同一账号后会自动合并。</p>
        <button v-if="supportsPasskey" class="primary-action account__wide" type="button" :disabled="busy" @click="loginWithPasskey">
          <KeyRound :size="17" />使用通行密钥登录
        </button>
        <div v-if="supportsPasskey" class="account__divider"><span>或使用密码</span></div>
        <form class="account__form" @submit.prevent="loginWithPassword">
          <input v-model="username" autocomplete="username" placeholder="用户名" aria-label="用户名" />
          <input v-model="password" type="password" autocomplete="current-password" placeholder="密码" aria-label="密码" />
          <button class="ghost-action account__wide" type="submit" :disabled="busy || !username || !password">登录</button>
        </form>
        <p v-if="error" class="account__error" role="alert">{{ error }}</p>
        <footer class="account__links">
          <button type="button" @click="view = 'register'">创建账号</button>
          <button type="button" @click="view = 'recovery'">忘记密码？使用恢复码</button>
        </footer>
      </div>

      <!-- Register -->
      <div v-else-if="view === 'register'" class="account__body">
        <p class="account__lead">此设备上已有的歌单会在创建后询问是否合并到账号。</p>
        <form class="account__form" @submit.prevent="createAccount">
          <input v-model="username" autocomplete="username" placeholder="用户名（3–32 位小写字母、数字、_ - .）" aria-label="用户名" />
          <input v-if="usePassword" v-model="password" type="password" autocomplete="new-password" placeholder="密码（至少 8 位）" aria-label="密码" />
          <div v-if="account.turnstileSiteKey" ref="turnstileBox" class="account__turnstile"></div>
          <button class="primary-action account__wide" type="submit" :disabled="busy || !username || (usePassword && password.length < 8)">
            <KeyRound v-if="!usePassword" :size="17" />{{ usePassword ? "创建账号" : "创建账号并设置通行密钥" }}
          </button>
        </form>
        <button v-if="supportsPasskey" class="account__switch" type="button" @click="usePassword = !usePassword">
          {{ usePassword ? "改用通行密钥（推荐，无需记密码）" : "此设备不支持？改用密码" }}
        </button>
        <p v-if="error" class="account__error" role="alert">{{ error }}</p>
        <footer class="account__links"><button type="button" @click="view = 'login'">已有账号？登录</button></footer>
      </div>

      <!-- Recovery -->
      <div v-else-if="view === 'recovery'" class="account__body">
        <p class="account__lead">每个恢复码只能使用一次。登录后请重新设置密码或通行密钥。</p>
        <form class="account__form" @submit.prevent="loginWithCode">
          <input v-model="username" autocomplete="username" placeholder="用户名" aria-label="用户名" />
          <input v-model="code" autocomplete="one-time-code" placeholder="恢复码，例如 abcde-fghjk" aria-label="恢复码" />
          <button class="primary-action account__wide" type="submit" :disabled="busy || !username || !code">登录</button>
        </form>
        <p v-if="error" class="account__error" role="alert">{{ error }}</p>
        <footer class="account__links"><button type="button" @click="view = 'login'">返回登录</button></footer>
      </div>

      <!-- Recovery codes -->
      <div v-else-if="view === 'codes'" class="account__body">
        <p class="account__lead">丢失通行密钥或忘记密码时，用这些恢复码登录。它们只显示这一次，请保存到安全的地方。</p>
        <p v-if="error" class="account__error" role="alert">{{ error }}</p>
        <ol class="account__codes">
          <li v-for="item in recoveryCodes" :key="item"><code>{{ item }}</code></li>
        </ol>
        <div class="account__row">
          <button class="ghost-action" type="button" @click="copyCodes">复制</button>
          <button class="primary-action" type="button" @click="finishCodes">我已保存</button>
        </div>
      </div>

      <!-- Merge -->
      <div v-else-if="view === 'merge' && plan" class="account__body">
        <p class="account__lead">此设备在登录前已有资料。合并后会出现在你的所有设备上，重复的歌单和歌曲不会重复添加。</p>
        <ul class="account__summary">
          <li v-if="plan.counts.created"><strong>{{ plan.counts.created }}</strong> 个新歌单</li>
          <li v-if="plan.merge.length"><strong>{{ plan.merge.length }}</strong> 个同名歌单补充歌曲</li>
          <li v-if="plan.counts.tracks"><strong>{{ plan.counts.tracks }}</strong> 首歌曲</li>
          <li v-if="plan.counts.taste"><strong>{{ plan.counts.taste }}</strong> 条喜欢／少推荐记录</li>
        </ul>
        <ul class="account__names">
          <li v-for="item in plan.create" :key="`c:${item.name}`">新增 · {{ item.name }} <small>{{ item.tracks.length }} 首</small></li>
          <li v-for="item in plan.merge" :key="`m:${item.cloudId}`">合并到 · {{ item.name }} <small>+{{ item.tracks.length }} 首</small></li>
        </ul>
        <div class="account__row">
          <button class="ghost-action" type="button" @click="account.resolveMerge(false)">只用云端</button>
          <button class="primary-action" type="button" @click="account.resolveMerge(true)">合并到账号</button>
        </div>
        <p class="account__hint">选择「只用云端」时，这些资料仍保存在本设备，退出登录后可以看到。</p>
      </div>

      <!-- Account -->
      <div v-else-if="view === 'account'" class="account__body">
        <div class="account__identity">
          <span class="account__avatar">{{ account.user?.username?.slice(0, 1).toUpperCase() }}</span>
          <span><strong>{{ account.user?.username }}</strong><small>{{ ({ synced: "已同步", syncing: "同步中…", offline: "离线，恢复网络后自动同步", expired: "登录已过期", error: account.syncMessage }[account.sync]) || "已登录" }}</small></span>
        </div>
        <p v-if="account.mode === 'expired'" class="account__note">登录已过期，修改已保存在本设备。<button type="button" class="account__inline" @click="view = 'login'">重新登录</button></p>

        <section class="account__section">
          <h3><ShieldCheck :size="16" />登录方式</h3>
          <button v-if="supportsPasskey" class="ghost-action" type="button" :disabled="busy" @click="createPasskey"><KeyRound :size="16" />在此设备添加通行密钥</button>
          <form class="account__form" @submit.prevent="savePassword">
            <input v-if="account.user?.has_password && !needsPassword" v-model="currentPassword" type="password" autocomplete="current-password" placeholder="当前密码" aria-label="当前密码" />
            <input v-model="newPassword" type="password" autocomplete="new-password" :placeholder="account.user?.has_password ? '新密码（至少 8 位）' : '设置密码（至少 8 位）'" aria-label="新密码" />
            <button class="ghost-action" type="submit" :disabled="busy || newPassword.length < 8">{{ account.user?.has_password ? "修改密码" : "设置密码" }}</button>
          </form>
        </section>

        <section class="account__section">
          <h3><Smartphone :size="16" />已登录的设备</h3>
          <ul class="account__devices">
            <li v-for="session in sessions" :key="session.id">
              <span><strong>{{ session.device_name }}</strong><small>{{ session.current ? "此设备" : relative(session.last_seen) }}</small></span>
              <button v-if="!session.current" class="ghost-action" type="button" :disabled="busy" @click="revoke(session)">移除</button>
            </li>
          </ul>
        </section>

        <p v-if="error" class="account__error" role="alert">{{ error }}</p>
        <section class="account__section account__section--end">
          <label class="account__check"><input v-model="clearLocal" type="checkbox" />清除此设备上的云端歌单副本</label>
          <button class="ghost-action account__logout" type="button" :disabled="busy" @click="signOut"><LogOut :size="16" />退出登录</button>
        </section>
      </div>
    </section>
  </div>
</template>

<style scoped>
.account { width: min(440px, 100%); padding: 0; }
.account__head { display: flex; align-items: center; justify-content: space-between; padding: 20px 22px 4px; }
.account__head h2 { margin: 0; font-size: 20px; }
.account__body { display: grid; gap: 14px; padding: 8px 22px 22px; }
.account__lead, .account__hint { margin: 0; color: var(--text-muted); font-size: 13px; line-height: 1.6; }
.account__hint { font-size: 12px; }
.account__note { margin: 0; padding: 10px 12px; border-radius: 12px; background: var(--accent-soft); color: var(--text); font-size: 13px; line-height: 1.5; }
.account__wide { justify-content: center; width: 100%; min-height: 44px; }
.account__form { display: grid; gap: 10px; }
.account__form input { height: 44px; padding: 0 14px; border: 1px solid var(--line); border-radius: 12px; color: var(--text); background: var(--input-bg); font: inherit; }
.account__divider { display: flex; align-items: center; gap: 10px; color: var(--text-muted); font-size: 12px; }
.account__divider::before, .account__divider::after { content: ""; flex: 1; height: 1px; background: var(--line); }
.account__links { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 8px; }
.account__links button, .account__switch, .account__inline { padding: 0; border: 0; background: none; color: var(--accent); font: inherit; font-size: 13px; cursor: pointer; }
.account__switch { justify-self: start; }
.account__error { margin: 0; color: var(--danger); font-size: 13px; }
.account__turnstile { min-height: 65px; }
.account__codes { display: grid; grid-template-columns: repeat(2, 1fr); gap: 8px; margin: 0; padding: 14px; border-radius: 14px; background: var(--surface-muted); list-style: none; }
.account__codes code { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 14px; letter-spacing: .02em; }
.account__row { display: flex; justify-content: flex-end; gap: 8px; }
.account__summary { display: grid; gap: 6px; margin: 0; padding: 12px 14px; border-radius: 14px; background: var(--surface); list-style: none; font-size: 14px; }
.account__summary strong { color: var(--accent); }
.account__names { max-height: 180px; margin: 0; padding: 0; overflow: auto; list-style: none; font-size: 13px; }
.account__names li { padding: 6px 0; border-bottom: 1px solid var(--line); overflow-wrap: anywhere; }
.account__names small { color: var(--text-muted); }
.account__identity { display: flex; align-items: center; gap: 12px; }
.account__identity > span:last-child { display: grid; gap: 2px; }
.account__identity small { color: var(--text-muted); font-size: 12px; }
.account__avatar { display: grid; place-items: center; width: 44px; height: 44px; border-radius: 50%; background: linear-gradient(180deg, #ff6b7f, #e8173f); color: #fff; font-size: 18px; font-weight: 750; }
.account__section { display: grid; gap: 10px; padding-top: 14px; border-top: 1px solid var(--line); }
.account__section h3 { display: flex; align-items: center; gap: 6px; margin: 0; font-size: 14px; }
.account__section--end { align-items: center; grid-template-columns: 1fr auto; }
.account__devices { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.account__devices li { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.account__devices li > span { display: grid; gap: 2px; min-width: 0; }
.account__devices strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 13px; }
.account__devices small { color: var(--text-muted); font-size: 12px; }
.account__check { display: flex; align-items: center; gap: 6px; color: var(--text-muted); font-size: 12px; }
.account__check input { accent-color: var(--accent); }
.account__logout { color: var(--danger); }
@media (max-width: 640px) {
  .account { max-height: calc(100dvh - 24px); overflow: auto; }
  .account__section--end { grid-template-columns: 1fr; }
}
</style>
