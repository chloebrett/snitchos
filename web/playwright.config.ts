import { defineConfig, devices } from "@playwright/test";

/// The browser tests are the only place three of this milestone's acceptance
/// criteria can be checked at all: that the guest actually boots to heartbeat, that
/// the tab stays responsive while it does, and that two loads produce byte-identical
/// output. None of those are reachable from the Rust suite or from
/// `wasm-pack test --node` — they are claims about a real browser.
export default defineConfig({
  testDir: "./e2e",
  // `@measurement` specs are standing measurements of known, unfixed problems, so
  // they are red by design and excluded from the default run — an acceptance suite
  // whose green includes a known failure is not saying anything.
  //
  // Conditional, because a config `grepInvert` composes with a CLI `--grep` as AND:
  // leaving it set unconditionally means `--grep @measurement` matches nothing, which
  // is exactly the "documented command does not work" trap this replaced. `yarn
  // measure` sets MEASURE=1 to lift it.
  ...(process.env.MEASURE ? {} : { grepInvert: /@measurement/ }),

  // Booting a kernel in an interpreter in wasm is not a 5-second operation, and CI
  // machines are slower than this one.
  timeout: 120_000,
  expect: { timeout: 30_000 },

  // A flake here would most likely be a real determinism bug, which is exactly what
  // we want to notice rather than paper over.
  retries: 0,
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? "list" : "html",

  use: {
    baseURL: "http://localhost:4173",
    trace: "retain-on-failure",
  },

  // `preview` serves the production build, which is what we want to test: the dev
  // server's transform pipeline is not what a visitor gets.
  webServer: {
    command: "yarn preview --port 4173 --strictPort",
    url: "http://localhost:4173",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },

  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
