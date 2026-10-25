<script setup>
import AccountButton from "../account/AccountButton.vue";
import ListeningSettings from "../player/ListeningSettings.vue";

defineProps({ title: { type: String, required: true } });
</script>

<template>
  <header class="page-header">
    <h1>{{ title }}</h1>
    <div class="page-header__actions">
      <slot />
      <!-- Phones have no sidebar or player toolbar: settings and the account
           live beside the large title, as in Apple Music. -->
      <ListeningSettings class="page-header__phone" compact placement="below" />
      <AccountButton class="page-header__phone" compact />
    </div>
  </header>
</template>
