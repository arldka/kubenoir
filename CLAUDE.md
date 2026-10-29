# Project Instructions: Kubernetes Narrative Detective Game

## Product Vision & Architecture
- **Concept**: A 100% open-source, browser-based SRE/Kubernetes detective game. Players investigate and debug simulated incidents via a web terminal (`xterm.js`).
- **Engine**: Rust compiled to WASM (`wasm-bindgen`). Runs a simulated in-browser Kubernetes control plane and SQLite state store using `k8s-openapi`.
- **Container apps**: what a container does is a real program, a template app in `apps/` (its own cargo workspace, `wasm32-wasip1`), run with `wasmi` against a virtual clock (`engine/src/apps/`). Apps are versioned artifacts (`name@version`, immutable once published): the browser engine is given them (`AppSet`), fetched from an app registry; native builds embed the local builds (`bundled-apps` feature, `engine/build.rs`). Scenarios pick a template per image and configure it; see `docs/apps.md`. Template config describes the app; behavior lives in the app's code, never in `when/then` tables. Networking is not simulated: an app mocks its connections, with the servers it can reach described in its image config (e.g. `db-client`'s `database`). Don't add host calls for sockets or cluster lookups.
- **Frontend**: SvelteKit + Tailwind CSS, powered by **Bun**. Renders narrative briefings, timers, scoreboards, and the interactive terminal in a vintage look.
- **Registries**: scenarios and apps are fetched over HTTP from configurable sources (`VITE_SCENARIO_SOURCES`, `VITE_APP_REGISTRIES`) with the same layout everywhere; the local registry (`make registry` → `frontend/static/registry/`) is the default and ships this repo's apps and scenarios. See `docs/registry.md`.
- **Scenarios & Community**: 
  - All scenarios, narrative lore, initial cluster states, and win/hidden validation rules are 100% open-source (stored as public JSON files).
- **Scoring & Verification**: Stateless verification logic checks player actions against open scenario assertions and records scores to Turso. The engine already judges a session (`success`, `findings`, `replay`); the scenario file format is documented in `docs/scenario-format.md`.

## Build & Test Commands
Use `make` (run `make` for the list); it sets `PATH` so rustup's toolchain wins. `make doctor` diagnoses a broken setup, `make ci` runs exactly what the GitHub workflow (`.github/workflows/ci.yml`) runs.
- **First time**: `make setup` (installs `wasm-pack` and the frontend packages); `cd frontend && bunx playwright install chromium` once for the browser tests.
- **Rust Engine**: `make test` (engine + frontend unit tests), `make lint` (rustfmt check + clippy as errors), `make fmt`
- **WASM Build + Bindings**: `make wasm` (= `wasm-pack build engine --target web --out-dir ../frontend/src/lib/wasm --no-default-features`, rebuilt only when `engine/` changes; the output is gitignored)
- **Frontend**: `make dev`, `make check` (types), `make build` (all build the local registry first)
- **Scenarios & apps**: `make apps`, `make registry`, `make scenario-check [SCENARIO=path]` (loads scenarios and plays their `solution.json`)
- **End-to-end (real browser)**: `make e2e` (builds, serves and plays the case in Chromium; uses a Chromium from `~/.cache/ms-playwright` or `$PLAYWRIGHT_CHROMIUM_PATH`)
- Plain `cargo`/`wasm-pack` also work as long as rustup's tools are the ones on `PATH` (`rust-toolchain.toml` pins the toolchain and adds the wasm target). A system-wide Rust install without the wasm target on `PATH` ahead of rustup breaks `wasm-pack`.

## Sound
- Web Audio, in `frontend/src/lib/audio/`. One long ambience track (`ambience.ogg`: jazz, vinyl, rain, breathing and smoking are all mixed into it) is looped with a crossfade, plus per-keystroke typewriter sounds (`key-*.ogg`). Audio files are optional and discovered by file name; see `frontend/src/lib/audio/assets/README.md` for names, format, loop guidance and the licensing/credits rule.
- Sound starts on the player's first click or key press (browser autoplay rules); master volume and mute are saved in localStorage.

## Coding Style & Guardrails
- **Separation of Concerns**: Keep simulation logic, command parsing, and rule validation strictly inside `/engine` (Rust). Frontend (`/frontend`) handles UI layout, stores, and terminal rendering using Bun.
- **Asynchronous WASM**: Initialize WASM asynchronously via an async wrapper store on the frontend.
- **Extensibility**: Design scenario loaders and parsers to be fully generic so they can ingest any valid open-source scenario JSON file dynamically.
- **Explanations**: Briefly outline complex state machine changes or WASM memory pointers before writing code.
