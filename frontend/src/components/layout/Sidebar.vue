<script setup>
import { Clock3, Disc3, HardDrive, ListMusic, Search, Settings2 } from "lucide-vue-next";
import { useStatusStore } from "../../stores/status";
import { usePlaylistStore } from "../../stores/playlists";
import AccountButton from "../account/AccountButton.vue";

defineProps({
  platforms: { type: Array, default: () => [] },
});

const status = useStatusStore();
const playlists = usePlaylistStore();
const emit = defineEmits(["playlist-contextmenu", "discover"]);

const navItems = [
  { key: "discover", label: "在线音乐", icon: Search },
  { key: "queue", label: "播放队列", icon: Disc3 },
  { key: "cache", label: "本地缓存", icon: HardDrive },
  { key: "playlists", label: "我的歌单", icon: ListMusic },
];

function switchView(key) {
  if (key === "discover") {
    emit("discover");
    return;
  }
  status.activeView = key;
  status.activePlaylistId = null;
  status.persist();
}

function openPlaylist(id) {
  status.activeView = "playlists";
  status.activePlaylistId = id;
  status.persist();
}
</script>

<template>
  <aside class="sidebar">
    <div class="brand">
      <div class="brand__mark" aria-hidden="true">
        <!-- Same sound-bar "M" as the app icon. -->
        <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor">
          <rect x="2" y="7.4" width="3" height="9.2" rx="1.5" />
          <rect x="6.25" y="4.8" width="3" height="14.4" rx="1.5" />
          <rect x="10.5" y="8.4" width="3" height="7.2" rx="1.5" />
          <rect x="14.75" y="4.8" width="3" height="14.4" rx="1.5" />
          <rect x="19" y="7.4" width="3" height="9.2" rx="1.5" />
        </svg>
      </div>
      <div>
        <div class="brand__name">MHub</div>
        <a class="brand__sub" href="/source.tar.gz" title="下载此版本源代码 · AGPL-3.0">开源音乐播放器</a>
      </div>
    </div>

    <nav class="nav-list" aria-label="主要导航">
      <button
        v-for="item in navItems"
        :key="item.key"
        type="button"
        class="nav-item"
        :class="{ 'is-active': status.activeView === item.key || (item.key === 'discover' && status.activeView === 'recommendation') }"
        :aria-current="status.activeView === item.key || (item.key === 'discover' && status.activeView === 'recommendation') ? 'page' : undefined"
        @click="switchView(item.key)"
      >
        <component :is="item.icon" :size="18" />
        <span>{{ item.label }}</span>
      </button>
    </nav>

    <div class="sidebar__section">
      <div class="sidebar__label">平台</div>
      <div class="platform-list">
        <button
          v-for="platform in platforms"
          :key="platform.id"
          class="platform-chip"
          :class="{ 'is-active': status.searchPlatform === platform.id }"
          @click="status.searchPlatform = platform.id; status.persist()"
        >
          {{ platform.label }}
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
          @click="openPlaylist(playlist.id)"
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
    <div class="sidebar__footer">
      <Clock3 :size="16" />
      <span>{{ status.notice }}</span>
      <Settings2 :size="16" />
    </div>
  </aside>
</template>
