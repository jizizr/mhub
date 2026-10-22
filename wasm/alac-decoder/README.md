# ALAC decoder WASM module

This crate exposes the packet decoder from [`alac`](https://crates.io/crates/alac)
to JavaScript through `wasm-bindgen`. It is deliberately a codec-only module:
it does not parse M4A/fMP4, perform network requests, decrypt media, or create
an `AudioContext`.

The streaming pipeline should perform these steps outside the module:

1. Parse the ALAC sample entry and pass its 24-byte magic cookie to
   `new AlacDecoder(cookie)`.
2. Extract one complete ALAC sample from each decrypted fMP4 fragment.
3. Call `decodePacketF32(packet, output)` and transfer the written
   `Float32Array` to an `AudioWorkletNode` ring buffer.

`decodePacketI32` is available when an application needs lossless integer PCM.
The Float32 method maps the left-aligned i32 samples to Web Audio's `[-1, 1)`
range without changing the source ALAC file or its cache.

The default `alac` MP4/CAF reader features are disabled. Those readers require
seekable input and are not appropriate for an append-only HTTP stream; the
packet decoder itself is small, pure Rust, and has an MIT/Apache-2.0 license.

## Build

Install the target once:

```sh
rustup target add wasm32-unknown-unknown
```

Build the optimized raw module and, when the `wasm-bindgen` CLI is installed,
generate browser bindings in `pkg/`:

```sh
./build.sh
```

The script uses a crate-local `target/` directory and never changes the root
Rust workspace. Install the CLI if needed:

```sh
cargo install wasm-bindgen-cli
```

`build.sh` also accepts `wasm-pack` as an alternative and will use its
matching `wasm-bindgen` CLI automatically when the standalone command is not
installed.

For production builds the script runs Binaryen's `wasm-opt -Oz` pass (with
bulk-memory enabled for current Rust targets) and copies the optimized module
to the frontend. If Binaryen is unavailable it keeps the valid release module
and prints an install hint; the decoder API and output remain unchanged.

The generated `.wasm` can be loaded from a dedicated Worker. Keep decoding out
of the main UI thread; the AudioWorklet should only consume the transferred PCM
blocks and handle its own bounded ring buffer/back-pressure.
