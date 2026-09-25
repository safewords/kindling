import { createApp } from "vue";
import { createRouter, createWebHistory } from "vue-router";

import App from "./App.vue";
import { session } from "./lib/api.js";
import { startLive } from "./lib/live.js";

import Dashboard from "./pages/Dashboard.vue";
import Devices from "./pages/Devices.vue";
import Device from "./pages/Device.vue";
import Policy from "./pages/Policy.vue";
import Config from "./pages/Config.vue";
import Events from "./pages/Events.vue";

const routes = [
  { path: "/", name: "dashboard", component: Dashboard, meta: { title: "Overview" } },
  { path: "/devices", name: "devices", component: Devices, meta: { title: "Machines" } },
  { path: "/devices/:mac", name: "device", component: Device, props: true, meta: { title: "Machine" } },
  { path: "/policy", name: "policy", component: Policy, meta: { title: "Policy" } },
  { path: "/config", name: "config", component: Config, meta: { title: "Configuration" } },
  { path: "/events", name: "events", component: Events, meta: { title: "Boot log" } },
  // Where the server-rendered pages used to live.
  { path: "/hosts", redirect: "/devices" },
  { path: "/rules", redirect: "/policy" },
];

const router = createRouter({
  // History mode: the server declares each of these paths, so a reload on
  // /policy serves the shell rather than a 404.
  history: createWebHistory(),
  routes,
  scrollBehavior: () => ({ top: 0 }),
});

const mount = document.getElementById("app");

// Handed over in the document, so the first screen renders without waiting on
// a round trip to find out what this server calls itself.
session.name = mount.dataset.name || session.name;
session.server = mount.dataset.server || "";
session.base = mount.dataset.base || "";
session.version = mount.dataset.version || "";

router.afterEach((to) => {
  document.title = to.meta.title ? `${to.meta.title} · ${session.name}` : session.name;
});

createApp(App).use(router).mount(mount);

// After mounting, so the first screen's own load is not racing the socket for
// the connection — and a server that refuses the upgrade still renders.
startLive();
