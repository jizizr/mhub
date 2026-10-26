<script setup>
import { onBeforeUnmount, onMounted, ref } from "vue";
import { Disc3, HardDrive, House, LibraryBig, ListMusic, Search } from "lucide-vue-next";
import { useStatusStore } from "../../stores/status";
import { usePlaylistStore } from "../../stores/playlists";
import AccountButton from "../account/AccountButton.vue";
import { attachLiquidGlass } from "../../utils/liquid-glass";

const status = useStatusStore();
const playlists = usePlaylistStore();
const emit = defineEmits(["playlist-contextmenu", "navigate"]);
// Only the phone tab bar is glass; the filter is built when CSS opts in.
const root = ref(null);
let detachGlass = () => {};
// Same liquid-glass preset as the floating player (attachLiquidGlass defaults).
onMounted(() => { detachGlass = attachLiquidGlass(root.value); });
onBeforeUnmount(() => detachGlass());

// Phones show the first group as a tab bar (主页 · 资料库 · 搜索); the
// desktop sidebar shows 搜索 · 主页 and lists the library sections instead.
const tabs = [
  { key: "discover", label: "主页", icon: House, active: ["discover", "recommendation"] },
  { key: "library", label: "资料库", icon: LibraryBig, active: ["playlists", "queue", "cache"], mobile: true },
  { key: "search", label: "搜索", icon: Search, active: ["search"] },
];
const library = [
  { key: "queue", label: "播放队列", icon: Disc3 },
  { key: "playlists", label: "歌单", icon: ListMusic },
  { key: "cache", label: "本地缓存", icon: HardDrive },
];

const isActive = (item) => (item.active || [item.key]).includes(status.activeView)
  && !(item.key === "playlists" && status.activePlaylistId);
</script>

<template>
  <aside ref="root" class="sidebar">
    <div class="brand">
      <div class="brand__mark" aria-hidden="true">
        <!-- Same sound-bar "M" as the app icon. -->
        <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
          <rect x="2" y="7.4" width="3" height="9.2" rx="1.5" />
          <rect x="6.25" y="4.8" width="3" height="14.4" rx="1.5" />
          <rect x="10.5" y="8.4" width="3" height="7.2" rx="1.5" />
          <rect x="14.75" y="4.8" width="3" height="14.4" rx="1.5" />
          <rect x="19" y="7.4" width="3" height="9.2" rx="1.5" />
        </svg>
      </div>
      <span class="brand__name">MHub</span>
    </div>

    <nav class="nav-list" aria-label="主要导航">
      <button
        v-for="item in tabs"
        :key="item.key"
        type="button"
        class="nav-item"
        :class="[`nav-item--${item.key}`, { 'nav-item--mobile': item.mobile, 'is-active': isActive(item) }]"
        :aria-current="isActive(item) ? 'page' : undefined"
        @click="emit('navigate', item.key)"
      >
        <component :is="item.icon" :size="18" />
        <span>{{ item.label }}</span>
      </button>
    </nav>

    <div class="sidebar__section">
      <div class="sidebar__label">资料库</div>
      <div class="nav-list">
        <button
          v-for="item in library"
          :key="item.key"
          type="button"
          class="nav-item"
          :class="{ 'is-active': isActive(item) }"
          :aria-current="isActive(item) ? 'page' : undefined"
          @click="emit('navigate', item.key)"
        >
          <component :is="item.icon" :size="18" />
          <span>{{ item.label }}</span>
        </button>
      </div>
    </div>

    <div v-if="playlists.count" class="sidebar__section sidebar__playlists">
      <div class="sidebar__label">歌单</div>
      <div class="playlist-nav-list">
        <button
          v-for="playlist in playlists.items"
          :key="playlist.id"
          class="platform-chip playlist-nav-item"
          :class="{ 'is-active': status.activePlaylistId === playlist.id }"
          @click="emit('navigate', 'playlist', playlist.id)"
          @contextmenu.prevent="emit('playlist-contextmenu', { event: $event, playlist })"
        >
          <span class="playlist-nav-item__cover">
            <img v-if="playlist.cover" :src="playlist.cover" alt="" loading="lazy" />
          </span>
          <span>{{ playlist.name }}</span>
        </button>
      </div>
    </div>

    <AccountButton class="sidebar__account" />
  </aside>
</template>
