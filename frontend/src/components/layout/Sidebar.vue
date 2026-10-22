<script setup>
import { Clock3, Disc3, HardDrive, ListMusic, Search, Settings2 } from "lucide-vue-next";
import { useStatusStore } from "../../stores/status";
import { usePlaylistStore } from "../../stores/playlists";

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
      <div class="brand__mark"><Disc3 :size="20" /></div>
      <div>
        <div class="brand__name">MHub</div>
        <a class="brand__sub" href="/source.tar.gz" title="下载此版本源代码 · AGPL-3.0">开源音乐播放器</a>
      </div>
    </div>

    <nav class="nav-list">
      <button
        v-for="item in navItems"
        :key="item.key"
        class="nav-item"
        :class="{ 'is-active': status.activeView === item.key || (item.key === 'discover' && status.activeView === 'recommendation') }"
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

    <div class="sidebar__footer">
      <Clock3 :size="16" />
      <span>{{ status.notice }}</span>
      <Settings2 :size="16" />
    </div>
  </aside>
</template>
