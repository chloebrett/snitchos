import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],

  // The emulator is a multi-MB wasm module. Two things follow:
  //
  //  - It is loaded as a URL (`?url` in `snemu.ts`) rather than imported as a
  //    module, so it stays a separately-fetched asset that can be instantiated by
  //    streaming. `assetsInlineLimit: 0` is belt-and-braces: nothing should ever be
  //    inlined into the JS bundle as base64, least of all this.
  //  - Its glue uses top-level await, so the build target must be modern enough to
  //    keep it rather than try to down-level it.
  build: {
    target: "es2022",
    assetsInlineLimit: 0,
  },

  test: {
    environment: "jsdom",
    setupFiles: ["./src/test-setup.ts"],
    // `e2e/` is Playwright's; Vitest must not try to run those specs in jsdom,
    // where there is no browser to drive.
    exclude: ["**/node_modules/**", "**/dist/**", "**/e2e/**"],
  },
});
