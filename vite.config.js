// The Vite config for a Rainier application, plus Vue and Tailwind.
//
// Three things matter to the framework's `@vite` directive:
//
//   1. The entries: what a template may name in `@vite([...])`.
//   2. The manifest: `public/build/manifest.json`, which resolves an entry to
//      its content-hashed output after `npm run build`.
//   3. The hot file: `public/hot`, holding the dev server's origin while
//      `npm run dev` runs — its presence is what flips the directive into
//      dev-server mode, and its removal flips it back.
//
// The `rainier()` plugin below is the whole backend integration.

import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import tailwindcss from "@tailwindcss/vite";
import { existsSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const hotFile = resolve("public/hot");

function rainier() {
  return {
    name: "rainier",
    configureServer(server) {
      server.httpServer?.once("listening", () => {
        const address = server.httpServer.address();
        const protocol = server.config.server.https ? "https" : "http";
        // A wildcard bind is not a reachable origin; the browser needs a real
        // host.
        const host =
          !address.address ||
          ["::", "0.0.0.0", "::1", "127.0.0.1"].includes(address.address)
            ? "localhost"
            : address.address;
        writeFileSync(hotFile, `${protocol}://${host}:${address.port}`);
      });

      // The file's absence is load-bearing — a stale hot file makes every page
      // point at a dev server that is gone.
      const clean = () => {
        if (existsSync(hotFile)) rmSync(hotFile);
      };
      process.on("exit", clean);
      for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
        process.on(signal, () => {
          clean();
          process.exit();
        });
      }
    },
  };
}

export default defineConfig({
  plugins: [rainier(), vue(), tailwindcss()],
  // Vite's own "copy this directory verbatim" feature, off. `public/` here is
  // the web root the framework reads `hot` and `build/` from, not a pile of
  // files to copy into the build — and leaving it on makes Vite warn that the
  // output directory sits inside it.
  publicDir: false,
  build: {
    // Where the framework's resolver and the `/build/{path*}` route look.
    outDir: "public/build",
    emptyOutDir: true,
    // By name, so the manifest lands at `public/build/manifest.json`.
    manifest: "manifest.json",
    rollupOptions: {
      // The closed set of `@vite` entries. An entry a template names that is
      // not listed here is a render-time error naming this file.
      input: ["resources/css/app.css", "resources/js/app.js"],
    },
  },
  server: {
    // The Rust server is the origin the browser is on; the dev server only
    // serves modules. Anything else would need the API proxied, and it does
    // not — the page is loaded from :8080 either way.
    cors: true,
  },
});
