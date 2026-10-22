/* tslint:disable */
/* eslint-disable */

/**
 * Stateful ALAC packet decoder.
 *
 * A decoder must be kept for the whole track because packet decoding uses the
 * stream's codec configuration and output frame size.  ALAC packets are
 * independent at the codec level, so a caller may feed packets as soon as
 * they arrive; container parsing and network buffering stay outside this
 * type.
 */
export class AlacDecoder {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Decode one complete ALAC packet into interleaved normalized Float32 PCM.
     *
     * Values are in the range expected by Web Audio (`[-1, 1)`).  The
     * conversion happens in Rust so an AudioWorklet only has to copy the
     * transferred Float32Array into its ring buffer.
     */
    decodePacketF32(packet: Uint8Array, output: Float32Array): number;
    /**
     * Decode one complete ALAC packet into interleaved signed 32-bit PCM.
     *
     * The decoded values are left-aligned in i32, matching the `alac` crate's
     * `Sample for i32` contract.  The returned count is the number of values
     * written, not the number of audio frames.  The caller should divide by
     * `channels` when advancing its playback clock.
     */
    decodePacketI32(packet: Uint8Array, output: Int32Array): number;
    /**
     * Maximum number of interleaved samples in one packet.
     */
    maxSamplesPerPacket(): number;
    /**
     * Create a decoder from an ALAC magic cookie.
     *
     * The cookie is the 24-byte `alac` decoder configuration from the sample
     * entry.  `StreamInfo::from_cookie` also accepts the optional `frma` and
     * `alac` atom wrappers used by some MP4 writers.
     */
    constructor(cookie: Uint8Array);
    /**
     * Source ALAC bit depth (normally 16 or 24).
     */
    readonly bitDepth: number;
    /**
     * Number of interleaved channels.
     */
    readonly channels: number;
    /**
     * Maximum decoded frames in one ALAC packet.
     */
    readonly frameLength: number;
    /**
     * Sample rate in Hz.
     */
    readonly sampleRate: number;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_alacdecoder_free: (a: number, b: number) => void;
    readonly alacdecoder_bitDepth: (a: number) => number;
    readonly alacdecoder_channels: (a: number) => number;
    readonly alacdecoder_decodePacketF32: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly alacdecoder_decodePacketI32: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly alacdecoder_frameLength: (a: number) => number;
    readonly alacdecoder_maxSamplesPerPacket: (a: number) => number;
    readonly alacdecoder_new: (a: number, b: number, c: number) => void;
    readonly alacdecoder_sampleRate: (a: number) => number;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export: (a: number, b: number) => number;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
