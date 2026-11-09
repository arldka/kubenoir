#!/usr/bin/env bash
# Checks the toolchain and says what to fix. Exits non-zero if a required tool is missing.
set -u

failed=0
ok() { printf '  ok       %-12s %s\n' "$1" "$2"; }
bad() { printf '  MISSING  %-12s %s\n' "$1" "$2"; failed=1; }
warn() { printf '  warning  %-12s %s\n' "$1" "$2"; }

echo "Toolchain"
if command -v rustup >/dev/null 2>&1; then
  ok rustup "$(rustup --version 2>/dev/null | head -1)"
else
  bad rustup "install it from https://rustup.rs"
fi

cargo_path="$(command -v cargo || true)"
case "$cargo_path" in
  "$HOME"/.cargo/*) ok cargo "$cargo_path" ;;
  "") bad cargo "not found; install rustup" ;;
  *) bad cargo "$cargo_path is not rustup's cargo; put ~/.cargo/bin first on PATH or remove the system Rust" ;;
esac

rustc_path="$(command -v rustc || true)"
case "$rustc_path" in
  "$HOME"/.cargo/*) ok rustc "$(rustc --version 2>/dev/null)" ;;
  "") bad rustc "not found" ;;
  *) bad rustc "$rustc_path is a system Rust without the wasm target (try: sudo pacman -Rns rust)" ;;
esac
if [ -x /usr/bin/rustc ]; then
  warn system-rust "/usr/bin/rustc exists; it can shadow rustup in shells where ~/.cargo/bin is not first"
fi

# Go only runs the upstream fidelity oracle (make oracle-goldens / make fidelity), never the game.
want_go="$(sed -n 's/^go //p' oracle/go.mod 2>/dev/null)"
if command -v go >/dev/null 2>&1; then
  ok go "$(go version) (oracle/go.mod wants $want_go)"
else
  warn go "not found; only needed for make oracle-goldens / make fidelity (Go $want_go)"
fi

# The engine targets wasm32-unknown-unknown; the template apps it embeds target wasm32-wasip1.
for target in wasm32-unknown-unknown wasm32-wasip1; do
  if rustup target list --installed 2>/dev/null | grep -qx "$target"; then
    ok wasm-target "$target"
  else
    bad wasm-target "run: rustup target add $target (rust-toolchain.toml does this on the next cargo run)"
  fi
done

if command -v clang >/dev/null 2>&1; then ok clang "$(clang --version | head -1)"; else bad clang "needed to compile SQLite for WASM"; fi
if command -v wasm-pack >/dev/null 2>&1; then ok wasm-pack "$(wasm-pack --version)"; else bad wasm-pack "run: make setup"; fi
if command -v bun >/dev/null 2>&1; then ok bun "$(bun --version)"; else bad bun "install from https://bun.sh"; fi

echo "End-to-end tests (optional)"
if [ -n "${PLAYWRIGHT_CHROMIUM_PATH:-}" ] && [ -x "$PLAYWRIGHT_CHROMIUM_PATH" ]; then
  ok chromium "$PLAYWRIGHT_CHROMIUM_PATH"
elif ls "$HOME"/.cache/ms-playwright/chromium-*/chrome-linux*/chrome >/dev/null 2>&1; then
  ok chromium "found in ~/.cache/ms-playwright"
else
  warn chromium "not found; run: cd frontend && bunx playwright install chromium"
fi

if [ "$failed" -eq 0 ]; then echo "All required tools are in place."; else echo "Fix the MISSING items above."; fi
exit "$failed"
