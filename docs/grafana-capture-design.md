# Programmatic Grafana capture (exploration thread)

> **Status:** exploration note, unbuilt. A separate thread off the diagram work —
> the two are siblings: both turn system state into shareable visual artifacts.
> Diagrams cover structure (crate graph, cap tree, span/switch folds); Grafana
> covers the *live metrics* view that a static diagram can't. This note captures
> how we'd automate pulling those views out as images.

## Motivation

Today the Grafana loop is manual: run the workload, open the dashboard, take a
screenshot, save it to a repo path (the assistant then reads the image). An
`xtask grafana shot` command would close that loop — capture a dashboard or a
single panel as a PNG on demand, for devlog posts, design review, or regression
comparison. It's the "extraction" sibling of `xtask diagram`.

## Mechanism: Grafana's server-side render API

Grafana can render a panel/dashboard to PNG server-side, but **only with the
`grafana-image-renderer`** — a headless-Chromium companion that core Grafana
does not bundle. So step one is a stack change.

- **Endpoints** (once the renderer is wired):
  - Single panel → `GET /render/d-solo/<uid>/<slug>?panelId=<n>&from=<t>&to=<t>&width=<w>&height=<h>&theme=<light|dark>` → PNG.
  - Full dashboard → `GET /render/d/<uid>/<slug>?from=<t>&to=<t>&kiosk&theme=<…>` → PNG.
- **Time range**: `from`/`to` as epoch-ms or relative (`now-1h`). Pin **absolute**
  ranges for reproducible devlog shots; relative for "latest".
- **Auth**: the dev stack runs `GF_AUTH_ANONYMOUS_ENABLED=true` +
  `GF_AUTH_ANONYMOUS_ORG_ROLE=Admin`, so the render API needs **no token
  locally**. A secured Grafana would use a service-account Bearer token.
- **Discovery**: `GET /api/search?type=dash-db` lists dashboards + uids; ours are
  provisioned (`stack/grafana/provisioning/`), so uids are stable and a command
  could resolve a friendly `--dashboard <name>` → uid.

## Stack change required

Add the renderer as a second compose service and point Grafana at it:

```yaml
renderer:
  image: grafana/grafana-image-renderer:latest
  ports: ["8081"]
# on the grafana service:
environment:
  - GF_RENDERING_SERVER_URL=http://renderer:8081/render
  - GF_RENDERING_CALLBACK_URL=http://grafana:3000/
```

Cost: another headless-Chromium container (~memory-hungry). Fine for local dev;
reconsider before wiring into CI.

## Command sketch

```
cargo xtask grafana shot --dashboard <uid|name> [--panel <n>] \
    [--from <t> --to <t>] [--theme dark] [--width 1000 --height 500] \
    --out <path.png>
```

- Shells the render URL via an HTTP client (confirm which one xtask already
  pulls in), writes the PNG.
- PNGs are **gitignored** (like the diagram SVGs) — render artifacts, not source.
- The assistant can `Read` the resulting PNG directly.

## Alternatives considered

- **`/api/ds/query`** returns raw series JSON — we could render our own charts
  (no Chromium), but that reinvents Grafana's rendering. The renderer path is
  right when the goal is *the Grafana view itself*.
- **Grafana snapshots** (`/api/snapshots`) freeze a dashboard's data into a
  shareable, self-contained URL — useful for pinning a devlog artifact, but still
  needs the renderer to become an image.

## Open questions

- Renderer resource cost — worth it in CI, or local-only?
- Reproducibility — pin absolute time ranges, or snapshot the underlying data
  first so the image is stable?
- Do we want per-panel shots (composable) or full-dashboard shots (context)?
- Naming/paths so captured PNGs slot cleanly into `posts/` devlog assets.

## Related

- `feedback_grafana_screenshots` (the current manual workflow this replaces).
- `docs/observability-design.md` (the telemetry the dashboards render).
- `docs/diagrams-design.md` (the structural-diagram sibling).
