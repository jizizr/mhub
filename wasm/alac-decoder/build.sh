#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
TARGET_DIR="$ROOT/target"
WASM="$TARGET_DIR/wasm32-unknown-unknown/release/alac_decoder.wasm"
# Keep local usernames and Cargo cache paths out of distributed panic strings.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$ROOT=/src/alac-decoder --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$HOME=/build"

cargo build \
  --manifest-path "$ROOT/Cargo.toml" \
  --target wasm32-unknown-unknown \
  --target-dir "$TARGET_DIR" \
  --release

if command -v wasm-bindgen >/dev/null 2>&1; then
  mkdir -p "$ROOT/pkg"
  wasm-bindgen "$WASM" \
    --target web \
    --typescript \
    --out-dir "$ROOT/pkg"
  echo "Generated WebAssembly bindings in $ROOT/pkg"
elif command -v wasm-pack >/dev/null 2>&1; then
  # wasm-pack installs/uses a matching wasm-bindgen CLI when one is not
  # already present.  Run from the crate directory because --out-dir is
  # intentionally relative to it.
  (
    cd "$ROOT"
    # wasm-pack's bundled wasm-opt can lag behind the Rust toolchain used to
    # build this crate (newer Rust emits bulk-memory instructions).  Disable
    # its implicit pass and run a compatible pass below instead.
    wasm-pack build . --target web --release --out-dir pkg --no-pack --no-opt
  )
  echo "Generated WebAssembly bindings in $ROOT/pkg"
else
  echo "Built $WASM"
  echo "Install wasm-bindgen-cli to generate browser bindings: cargo install wasm-bindgen-cli"
fi

# wasm-pack normally runs wasm-opt with a generic -O pass.  Run the size
# focused -Oz pass ourselves and explicitly enable bulk-memory so the output
# stays valid with current Rust/LLVM targets.  The lookup also covers the
# wasm-opt binary wasm-pack downloads into its local cache when no system
# binary is installed; if neither is available we keep the valid unoptimized
# module and print a clear hint.
WASM_OPT_BIN=""
if command -v wasm-opt >/dev/null 2>&1; then
  WASM_OPT_BIN=$(command -v wasm-opt)
else
  for CACHE_ROOT in \
    "${WASM_PACK_CACHE_DIR:-}" \
    "${HOME:-}/Library/Caches/.wasm-pack" \
    "${HOME:-}/.cache/.wasm-pack"
  do
    [ -n "$CACHE_ROOT" ] || continue
    for CANDIDATE in "$CACHE_ROOT"/wasm-opt-*/bin/wasm-opt; do
      if [ -x "$CANDIDATE" ]; then
        WASM_OPT_BIN="$CANDIDATE"
        break 2
      fi
    done
  done
fi

WASM_OUTPUT="$ROOT/pkg/alac_decoder_bg.wasm"
if [ -f "$WASM_OUTPUT" ] && [ -n "$WASM_OPT_BIN" ]; then
  WASM_OPT_TMP="$WASM_OUTPUT.opt"
  if "$WASM_OPT_BIN" --enable-bulk-memory -Oz "$WASM_OUTPUT" -o "$WASM_OPT_TMP" \
      || "$WASM_OPT_BIN" -Oz "$WASM_OUTPUT" -o "$WASM_OPT_TMP"; then
    mv "$WASM_OPT_TMP" "$WASM_OUTPUT"
    echo "Optimized $WASM_OUTPUT with wasm-opt -Oz"
  else
    rm -f "$WASM_OPT_TMP"
    echo "Warning: wasm-opt failed; keeping the valid unoptimized module" >&2
  fi
elif [ -f "$WASM_OUTPUT" ]; then
  echo "wasm-opt not found; keeping $WASM_OUTPUT unoptimized (install Binaryen for -Oz)" >&2
fi

# Keep the checked-in frontend import in sync when this crate is built from
# the monorepo. Standalone users still get the normal `pkg/` output above.
FRONTEND_DEST="$ROOT/../../frontend/src/wasm/alac-decoder"
if [ -d "$FRONTEND_DEST" ] && [ -f "$ROOT/pkg/alac_decoder.js" ] && [ -f "$ROOT/pkg/alac_decoder_bg.wasm" ]; then
  cp "$ROOT/pkg/alac_decoder.js" "$ROOT/pkg/alac_decoder_bg.wasm" "$ROOT/pkg/alac_decoder.d.ts" "$FRONTEND_DEST/"
  echo "Synchronized frontend bindings in $FRONTEND_DEST"
fi
