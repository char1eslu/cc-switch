# Action Warning Backlog

## 2026-06-17 macOS ad hoc build

- Run: https://github.com/char1eslu/cc-switch/actions/runs/27672928155
- Workflow: Build macOS Ad Hoc
- Head SHA: `5d8d7b69ac035e0d1411f37adf53e58168f84133`
- Result: success
- Artifact: https://github.com/char1eslu/cc-switch/actions/runs/27672928155/artifacts/7688812161
- Artifact size: 16,125,747 bytes
- Artifact SHA256: `715b13c18029ff620be38bad9c52a1e695fc8bd9670810a534613f60b5537232`

### Tooling and CI

- `pnpm/action-setup` reported a PNPM_HOME layout warning:
  `Detected a pnpm v10 installation layout at PNPM_HOME... pnpm v11 expects bins in PNPM_HOME/bin. Run "pnpm setup" to migrate your PATH to the v11 layout.`
- `pnpm install` reported ignored build scripts for `esbuild` and `msw`; decide whether to run `pnpm approve-builds` and commit the resulting policy.
- The pnpm cache post-job step reported:
  `Path Validation Error: Path(s) specified in the action for caching do(es) not exist`.
  The macOS workflow should probably use `pnpm store path --silent`, matching the CI workflow, instead of hard-coding `~/.local/share/pnpm/store`.

### Frontend Build

- `baseline-browser-mapping` data is over two months old. Update the relevant dependency or lockfile.
- Browserslist / `caniuse-lite` data is seven months old. Run the normal Browserslist DB update flow and review lockfile changes.
- Vite reported that `src/lib/api/subscription.ts` is both dynamically imported by `src/components/UsageScriptModal.tsx` and statically imported by `src/lib/api/index.ts` / `src/lib/query/subscription.ts`, so the dynamic import cannot move it into a separate chunk.
- Vite reported chunks larger than 500 kB after minification. The largest emitted JS chunk was `index-BFSaCGYM.js` at 3,464.47 kB, gzip 1,049.98 kB. Review code splitting and `manualChunks` before suppressing this warning.

### Rust Build

`cc-switch` lib generated 13 warnings:

- `src/services/config.rs:1:58`: unused import `ProviderService`.
- `src/services/provider/live.rs:5:18`: unused import `json`.
- `src/commands/misc.rs:1378:28`: unused variable `tool`.
- `src/proxy/forwarder.rs:2452:25`: unused variable `endpoint`.
- `src/proxy/handlers.rs:282:5`: unused variable `original_body`.
- `src/commands/misc.rs:840:10`: unused function `fetch_github_latest_version`.
- `src/commands/misc.rs:863:10`: unused function `fetch_pypi_latest_version`.
- `src/commands/misc.rs:1208:4`: unused function `push_env_single_dir`.
- `src/commands/misc.rs:1214:4`: unused function `extend_from_path_list`.
- `src/commands/misc.rs:3109:15`: unused function `launch_terminal_running`.
- `src/proxy/forwarder.rs:2343:4`: unused function `merge_query_params`.
- `src/services/provider/mod.rs:620:8`: unused associated functions `check_live_config_exists`, `provider_live_config_managed`, and `set_provider_live_config_managed`.
- `src/services/provider/live.rs:31:15`: unused function `provider_exists_in_live_config`.

Cargo suggested: `cargo fix --lib -p cc-switch` for five of the warnings, but review manually before applying broad fixes.
