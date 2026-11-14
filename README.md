# Kubenoire

A browser-based Kubernetes detective game. You are the SRE on a noir case: read the briefing,
investigate a broken cluster from a web terminal, and fix it before the 15 minutes are up.

Everything runs in your browser. The engine is Rust compiled to WASM: a simulated control plane
(ported from upstream Kubernetes validation, defaulting and `kubectl` behavior) that runs the
containers' programs as WASM apps. No real cluster, no backend.

**Live demo:** _add the workers.dev URL here after the first deploy_

## Quick start

```sh
make setup   # wasm-pack and the frontend packages
make dev     # builds the engine and the local registry, starts the dev server
make ci      # everything the GitHub workflow runs
```

`make doctor` diagnoses a broken toolchain. See `make` for all targets.

## Layout

| folder | what |
|---|---|
| `engine/` | the simulator, `kubectl`, judging (Rust, WASM) |
| `apps/` | template container apps (`wasm32-wasip1`) |
| `scenarios/` | one folder per case: story, rules and the cluster as YAML |
| `frontend/` | SvelteKit UI and terminal (Bun) |
| `oracle/` | Go reference run against upstream Kubernetes to generate test goldens (never shipped) |
| `docs/` | [scenario format](docs/scenario-format.md), [apps](docs/apps.md), [registries](docs/registry.md) |

## Write a case

A case is a folder in `scenarios/` with a `scenario.json` and the YAML it lists. Start from
[docs/scenario-format.md](docs/scenario-format.md), then check it with
`make scenario-check SCENARIO=scenarios/<id>/scenario.json`.

## Deploying

The build is static (`make build` → `frontend/build/`). The demo is a Cloudflare Worker serving
those files (`frontend/wrangler.jsonc`); CI deploys it from `main` after all checks pass, using the
`CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` repository secrets. `make preview-worker` serves
the build locally the same way. `VITE_*` settings are build-time; a remote registry must send CORS
headers (see [docs/registry.md](docs/registry.md)).

## License

MIT, see [LICENSE](LICENSE). Sound credits: `frontend/src/lib/audio/assets/CREDITS.md`.
