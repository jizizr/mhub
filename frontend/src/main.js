import { createApp } from "vue";
import { createPinia } from "pinia";
import App from "./App.vue";
import { registerPwa } from "./pwa";
import "./styles.css";

createApp(App).use(createPinia()).mount("#root");
registerPwa();
