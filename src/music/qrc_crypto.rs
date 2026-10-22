//! QQ QRC compatibility cipher, adapted from HyperLyrics-Enhanced's
//! TripleDesCustom.kt at 44775f583c8bb2161705cac77b2618e2c202bf57.
//! GPL-3.0; see docs/third-party/HyperLyrics-Enhanced-LICENSE.
//! This format uses word-reversed byte order, modified substitution tables,
//! and a custom key schedule. It is NOT standard 3DES.

const SBOX: [[u8; 64]; 8] = [
    [
        14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7, 0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12,
        11, 9, 5, 3, 8, 4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0, 15, 12, 8, 2, 4, 9,
        1, 7, 5, 11, 3, 14, 10, 0, 6, 13,
    ],
    [
        15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10, 3, 13, 4, 7, 15, 2, 8, 15, 12, 0, 1,
        10, 6, 9, 11, 5, 0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15, 13, 8, 10, 1, 3, 15,
        4, 2, 11, 6, 7, 12, 0, 5, 14, 9,
    ],
    [
        10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8, 13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5,
        14, 12, 11, 15, 1, 13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7, 1, 10, 13, 0, 6,
        9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12,
    ],
    [
        7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15, 13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2,
        12, 1, 10, 14, 9, 10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4, 3, 15, 0, 6, 10,
        10, 13, 8, 9, 4, 5, 11, 12, 7, 2, 14,
    ],
    [
        2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9, 14, 11, 2, 12, 4, 7, 13, 1, 5, 0, 15,
        10, 3, 9, 8, 6, 4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14, 11, 8, 12, 7, 1, 14,
        2, 13, 6, 15, 0, 9, 10, 4, 5, 3,
    ],
    [
        12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11, 10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13,
        14, 0, 11, 3, 8, 9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6, 4, 3, 2, 12, 9, 5,
        15, 10, 11, 14, 1, 7, 6, 0, 8, 13,
    ],
    [
        4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1, 13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5,
        12, 2, 15, 8, 6, 1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2, 6, 11, 13, 8, 1, 4,
        10, 7, 9, 5, 0, 15, 14, 2, 3, 12,
    ],
    [
        13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7, 1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6,
        11, 0, 14, 9, 2, 7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8, 2, 1, 14, 7, 4, 10,
        8, 13, 15, 12, 9, 0, 3, 5, 6, 11,
    ],
];

const PC1: [u8; 56] = [
    56, 48, 40, 32, 24, 16, 8, 0, 57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59,
    51, 43, 35, 62, 54, 46, 38, 30, 22, 14, 6, 61, 53, 45, 37, 29, 21, 13, 5, 60, 52, 44, 36, 28,
    20, 12, 4, 27, 19, 11, 3,
];
const PC2: [u8; 48] = [
    13, 16, 10, 23, 0, 4, 2, 27, 14, 5, 20, 9, 22, 18, 11, 3, 25, 7, 15, 6, 26, 19, 12, 1, 40, 51,
    30, 36, 46, 54, 29, 39, 50, 44, 32, 47, 43, 48, 38, 55, 33, 52, 45, 41, 49, 35, 28, 31,
];
const P: [u8; 32] = [
    15, 6, 19, 20, 28, 11, 27, 16, 0, 14, 22, 25, 4, 17, 30, 9, 1, 7, 23, 13, 31, 26, 2, 8, 18, 12,
    29, 5, 21, 10, 3, 24,
];

fn swap_words(bytes: &[u8]) -> u64 {
    u64::from_be_bytes([
        bytes[3], bytes[2], bytes[1], bytes[0], bytes[7], bytes[6], bytes[5], bytes[4],
    ])
}

fn schedule(key: &[u8], decrypt: bool) -> [u64; 16] {
    let key = swap_words(key);
    let mut halves = [0u32; 2];
    for (half, positions) in halves.iter_mut().zip(PC1.chunks(28)) {
        for (index, position) in positions.iter().enumerate() {
            *half |= (((key >> (63 - position)) & 1) as u32) << (31 - index);
        }
    }
    let mut rounds = [0; 16];
    for (round, shift) in [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1]
        .iter()
        .enumerate()
    {
        for half in &mut halves {
            *half = ((*half << shift) | (*half >> (28 - shift))) & 0xfffffff0;
        }
        let mut subkey = 0u64;
        for (index, position) in PC2.iter().enumerate() {
            let bit = if index < 24 {
                (halves[0] >> (31 - position)) & 1
            } else {
                // Upstream's offset is 27, not standard DES's 28.
                (halves[1] >> (31 - (position - 27))) & 1
            };
            subkey = (subkey << 1) | u64::from(bit);
        }
        rounds[if decrypt { 15 - round } else { round }] = subkey;
    }
    rounds
}

fn round_function(right: u32, key: u64) -> u32 {
    let mut expanded = 0u64;
    for group in 0..8 {
        for offset in 0..6 {
            let position = (group * 4 + offset + 31) % 32;
            expanded = (expanded << 1) | u64::from((right >> (31 - position)) & 1);
        }
    }
    expanded ^= key;
    let mut substituted = 0u32;
    for (index, table) in SBOX.iter().enumerate() {
        let bits = ((expanded >> (42 - index * 6)) & 63) as usize;
        let position = (bits & 32) | ((bits & 31) >> 1) | ((bits & 1) << 4);
        substituted = (substituted << 4) | u32::from(table[position]);
    }
    P.iter().fold(0, |result, position| {
        (result << 1) | ((substituted >> (31 - position)) & 1)
    })
}

fn block(bytes: &[u8], keys: &[u64; 16]) -> [u8; 8] {
    let input = swap_words(bytes);
    // Initial permutation, represented as zero-based bit positions.
    let positions: Vec<_> = [57, 59, 61, 63, 56, 58, 60, 62]
        .iter()
        .flat_map(|start| (0..8).map(move |column| start - 8 * column))
        .collect();
    let permuted = positions.iter().fold(0u64, |value, position| {
        (value << 1) | ((input >> (63 - position)) & 1)
    });
    let mut left = (permuted >> 32) as u32;
    let mut right = permuted as u32;
    for key in keys {
        (left, right) = (right, left ^ round_function(right, *key));
    }
    let joined = (u64::from(right) << 32) | u64::from(left);
    let mut output = 0u64;
    for (index, position) in positions.iter().enumerate() {
        output |= ((joined >> (63 - index)) & 1) << (63 - position);
    }
    let bytes = output.to_be_bytes();
    [
        bytes[3], bytes[2], bytes[1], bytes[0], bytes[7], bytes[6], bytes[5], bytes[4],
    ]
}

pub(super) fn decrypt(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() % 8 != 0 {
        return None;
    }
    let key = b"!@#)(*$%123ZXC!@!@#)(NHL";
    let schedules = [
        schedule(&key[16..24], true),
        schedule(&key[8..16], false),
        schedule(&key[..8], true),
    ];
    let mut output = Vec::with_capacity(input.len());
    for bytes in input.chunks_exact(8) {
        let mut value: [u8; 8] = bytes.try_into().ok()?;
        for keys in &schedules {
            value = block(&value, keys);
        }
        output.extend_from_slice(&value);
    }
    Some(output)
}
