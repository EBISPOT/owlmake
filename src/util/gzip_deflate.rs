//! The compressor behind `om gzip`: DEFLATE with lazy matching over a 32K
//! sliding window, dynamic/static/stored block selection, and the gzip CRC.
//!
//! The output is a function of the input bytes alone. Every threshold is
//! fixed — the level-6 match parameters, the 32K window, the block-flush
//! heuristics — so the same bytes always deflate to the same stream.
//!
//! The pieces, in the order the data flows:
//!
//!   * [`Deflater`] — the sliding window, the hash chains and the lazy
//!     match evaluation; it emits literals and (distance, length) pairs.
//!   * [`Trees`] — buffers those items for the current block, builds the
//!     Huffman trees, and decides whether the block goes out stored, with
//!     the static trees, or with dynamic trees.
//!   * [`BitWriter`] — packs codes LSB-first into bytes.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Sliding window size; a match may reach back at most `MAX_DIST` bytes.
const WSIZE: usize = 0x8000;
const WMASK: usize = WSIZE - 1;
/// The window buffer holds two window-fuls; the upper half slides down into
/// the lower half when the upper one fills.
const WINDOW_SIZE: usize = 2 * WSIZE;

const HASH_BITS: u32 = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: usize = HASH_SIZE - 1;

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// Bytes that must be ahead of `strstart` to look for a match (a longest
/// match plus the string inserted after it).
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const MAX_DIST: usize = WSIZE - MIN_LOOKAHEAD;
/// A length-3 match further back than this is not worth its distance code.
const TOO_FAR: usize = 4096;
/// Hash shift: after `MIN_MATCH` updates the first byte has left the hash.
const H_SHIFT: u32 = (HASH_BITS + MIN_MATCH as u32 - 1) / MIN_MATCH as u32;
/// Tail of a hash chain; window index 0 never matches, which keeps the
/// sentinel unambiguous.
const NIL: u16 = 0;

/// Level-6 match parameters.
///
/// `GOOD_LENGTH`: with a previous match at least this long, the chain search
/// is cut to a quarter. `MAX_LAZY`: a previous match at least this long is
/// emitted without looking for a better one at the next byte. `NICE_LENGTH`:
/// stop searching once a match this long is found. `MAX_CHAIN`: hash chain
/// entries examined per search.
const GOOD_LENGTH: usize = 8;
const MAX_LAZY: usize = 16;
const NICE_LENGTH: usize = 128;
const MAX_CHAIN: usize = 128;

const MAX_BITS: usize = 15;
const MAX_BL_BITS: usize = 7;
const LENGTH_CODES: usize = 29;
const LITERALS: usize = 256;
const END_BLOCK: usize = 256;
const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
const D_CODES: usize = 30;
const BL_CODES: usize = 19;
const HEAP_SIZE: usize = 2 * L_CODES + 1;

/// A block is flushed once this many items are buffered (minus one: the
/// buffer index must never reach the size).
const LIT_BUFSIZE: usize = 0x8000;
const DIST_BUFSIZE: usize = LIT_BUFSIZE;

const REP_3_6: usize = 16;
const REPZ_3_10: usize = 17;
const REPZ_11_138: usize = 18;

const STORED_BLOCK: u32 = 0;
const STATIC_TREES: u32 = 1;
const DYN_TREES: u32 = 2;

const EXTRA_LBITS: [u8; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const EXTRA_DBITS: [u8; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const EXTRA_BLBITS: [u8; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
/// The order in which bit-length code lengths are transmitted.
const BL_ORDER: [u8; BL_CODES] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Deflate `input` into the raw DEFLATE stream a gzip member carries between
/// its header and its trailer.
pub fn deflate_bytes(input: &[u8]) -> Vec<u8> {
    let mut d = Deflater::new(input);
    d.deflate();
    d.trees.out.finish()
}

/// CRC-32 (reflected, polynomial 0xEDB88320) of `data`, as the gzip trailer
/// records it.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffff_ffff
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

/// A complete single-member gzip file for `input`.
///
/// The header is `1f 8b 08 FLG MTIME(4, LE) XFL=0 OS=3`, followed by `name`
/// NUL-terminated when one is given (FLG then carries the FNAME bit); the
/// trailer is the CRC-32 of `input` and its length modulo 2^32, both
/// little-endian.
pub fn gzip_member(input: &[u8], mtime: u32, name: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() / 3 + 32);
    out.extend_from_slice(&[0x1f, 0x8b, 8, if name.is_some() { 0x08 } else { 0 }]);
    out.extend_from_slice(&mtime.to_le_bytes());
    out.extend_from_slice(&[0, 3]);
    if let Some(n) = name {
        out.extend_from_slice(n);
        out.push(0);
    }
    out.extend_from_slice(&deflate_bytes(input));
    out.extend_from_slice(&crc32(input).to_le_bytes());
    out.extend_from_slice(&(input.len() as u32).to_le_bytes());
    out
}

// ---------------------------------------------------------------------------
// Bit writer
// ---------------------------------------------------------------------------

struct BitWriter {
    out: Vec<u8>,
    /// Bits not yet written, LSB first.
    bi_buf: u16,
    /// How many of `bi_buf`'s bits are in use.
    bi_valid: u32,
    /// Bits sent so far; the block bookkeeping is checked against it.
    bits_sent: u64,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter { out: Vec::new(), bi_buf: 0, bi_valid: 0, bits_sent: 0 }
    }

    fn put_byte(&mut self, b: u8) {
        self.out.push(b);
    }

    fn put_short(&mut self, w: u16) {
        self.out.extend_from_slice(&w.to_le_bytes());
    }

    /// Append the low `length` bits of `value` (1..=15 of them).
    fn send_bits(&mut self, value: u32, length: u32) {
        debug_assert!(length > 0 && length <= 15);
        self.bits_sent += length as u64;
        if self.bi_valid > 16 - length {
            self.bi_buf |= (value << self.bi_valid) as u16;
            let w = self.bi_buf;
            self.put_short(w);
            self.bi_buf = (value as u16) >> (16 - self.bi_valid);
            self.bi_valid = self.bi_valid + length - 16;
        } else {
            self.bi_buf |= (value << self.bi_valid) as u16;
            self.bi_valid += length;
        }
    }

    /// Flush the pending bits, padding the last byte with zeros.
    fn bi_windup(&mut self) {
        if self.bi_valid > 8 {
            let w = self.bi_buf;
            self.put_short(w);
        } else if self.bi_valid > 0 {
            let b = self.bi_buf as u8;
            self.put_byte(b);
        }
        self.bi_buf = 0;
        self.bi_valid = 0;
        self.bits_sent = (self.bits_sent + 7) & !7;
    }

    /// Write `block` byte-aligned, preceded by the LEN/NLEN words when
    /// `header` is set.
    fn copy_block(&mut self, block: &[u8], header: bool) {
        self.bi_windup();
        if header {
            let len = block.len() as u16;
            self.put_short(len);
            self.put_short(!len);
            self.bits_sent += 2 * 16;
        }
        self.bits_sent += (block.len() as u64) << 3;
        self.out.extend_from_slice(block);
    }

    fn finish(mut self) -> Vec<u8> {
        self.bi_windup();
        self.out
    }
}

/// Reverse the low `len` bits of `code`.
fn bi_reverse(mut code: u32, mut len: u32) -> u32 {
    let mut res = 0u32;
    loop {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
        if len == 0 {
            break;
        }
    }
    res >> 1
}

// ---------------------------------------------------------------------------
// Huffman trees and block emission
// ---------------------------------------------------------------------------

/// One tree node. `fc` is the frequency while the tree is being built and
/// the bit string afterwards; `dl` is the parent index while the tree is
/// being built and the code length afterwards. Each pair shares one slot
/// on purpose: `gen_bitlen` reads a node's parent and writes its length in
/// the same pass.
#[derive(Clone, Copy, Default)]
struct Node {
    fc: u16,
    dl: u16,
}

/// What a tree is made of: its extra-bits table, the first code that carries
/// extra bits, how many codes it has, and the longest code allowed.
struct TreeDesc {
    extra_bits: &'static [u8],
    extra_base: usize,
    elems: usize,
    max_length: usize,
}

const L_DESC: TreeDesc = TreeDesc {
    extra_bits: &EXTRA_LBITS,
    extra_base: LITERALS + 1,
    elems: L_CODES,
    max_length: MAX_BITS,
};
const D_DESC: TreeDesc =
    TreeDesc { extra_bits: &EXTRA_DBITS, extra_base: 0, elems: D_CODES, max_length: MAX_BITS };
const BL_DESC: TreeDesc =
    TreeDesc { extra_bits: &EXTRA_BLBITS, extra_base: 0, elems: BL_CODES, max_length: MAX_BL_BITS };

/// Scratch state shared by every tree construction.
struct TreeBuild {
    bl_count: [u16; MAX_BITS + 1],
    heap: [usize; HEAP_SIZE],
    heap_len: usize,
    heap_max: usize,
    depth: [u8; HEAP_SIZE],
    /// Bit length of the current block with the dynamic trees.
    opt_len: u64,
    /// Bit length of the current block with the static trees.
    static_len: u64,
}

const SMALLEST: usize = 1;

impl TreeBuild {
    /// `n` orders before `m`: lower frequency, or equal frequency and no
    /// deeper.
    fn smaller(&self, tree: &[Node], n: usize, m: usize) -> bool {
        tree[n].fc < tree[m].fc || (tree[n].fc == tree[m].fc && self.depth[n] <= self.depth[m])
    }

    /// Restore the heap property below `k`.
    fn pqdownheap(&mut self, tree: &[Node], mut k: usize) {
        let v = self.heap[k];
        let mut j = k << 1;
        while j <= self.heap_len {
            if j < self.heap_len && self.smaller(tree, self.heap[j + 1], self.heap[j]) {
                j += 1;
            }
            if self.smaller(tree, v, self.heap[j]) {
                break;
            }
            self.heap[k] = self.heap[j];
            k = j;
            j <<= 1;
        }
        self.heap[k] = v;
    }

    /// Compute the code lengths of a built tree, limiting them to the tree's
    /// maximum by moving leaves down, and accumulate `opt_len` and
    /// `static_len`.
    fn gen_bitlen(&mut self, tree: &mut [Node], stree: Option<&[Node]>, desc: &TreeDesc, max_code: usize) {
        self.bl_count = [0; MAX_BITS + 1];
        let max_length = desc.max_length;
        let mut overflow = 0i32;

        tree[self.heap[self.heap_max]].dl = 0;
        let mut h = self.heap_max + 1;
        while h < HEAP_SIZE {
            let n = self.heap[h];
            h += 1;
            let mut bits = tree[tree[n].dl as usize].dl as usize + 1;
            if bits > max_length {
                bits = max_length;
                overflow += 1;
            }
            tree[n].dl = bits as u16;
            if n > max_code {
                continue;
            }
            self.bl_count[bits] += 1;
            let xbits = if n >= desc.extra_base { desc.extra_bits[n - desc.extra_base] as u64 } else { 0 };
            let f = tree[n].fc as u64;
            self.opt_len = self.opt_len.wrapping_add(f * (bits as u64 + xbits));
            if let Some(s) = stree {
                self.static_len = self.static_len.wrapping_add(f * (s[n].dl as u64 + xbits));
            }
        }
        if overflow == 0 {
            return;
        }

        loop {
            let mut bits = max_length - 1;
            while self.bl_count[bits] == 0 {
                bits -= 1;
            }
            self.bl_count[bits] -= 1;
            self.bl_count[bits + 1] += 2;
            self.bl_count[max_length] -= 1;
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }

        let mut bits = max_length;
        while bits != 0 {
            let mut n = self.bl_count[bits];
            while n != 0 {
                h -= 1;
                let m = self.heap[h];
                if m > max_code {
                    continue;
                }
                if tree[m].dl as usize != bits {
                    let delta = (bits as i64 - tree[m].dl as i64) * tree[m].fc as i64;
                    self.opt_len = self.opt_len.wrapping_add(delta as u64);
                    tree[m].dl = bits as u16;
                }
                n -= 1;
            }
            bits -= 1;
        }
    }

    /// Assign canonical codes from the code lengths in `bl_count`.
    fn gen_codes(&self, tree: &mut [Node], max_code: usize) {
        let mut next_code = [0u16; MAX_BITS + 1];
        let mut code = 0u16;
        for bits in 1..=MAX_BITS {
            code = (code.wrapping_add(self.bl_count[bits - 1])) << 1;
            next_code[bits] = code;
        }
        for n in tree.iter_mut().take(max_code + 1) {
            let len = n.dl as u32;
            if len == 0 {
                continue;
            }
            n.fc = bi_reverse(next_code[len as usize] as u32, len) as u16;
            next_code[len as usize] += 1;
        }
    }

    /// Build the Huffman tree for the frequencies in `tree` and return the
    /// largest code with a non-zero frequency.
    fn build_tree(&mut self, tree: &mut [Node], stree: Option<&[Node]>, desc: &TreeDesc) -> usize {
        let elems = desc.elems;
        let mut max_code: isize = -1;
        let mut node = elems;

        self.heap_len = 0;
        self.heap_max = HEAP_SIZE;
        for n in 0..elems {
            if tree[n].fc != 0 {
                self.heap_len += 1;
                self.heap[self.heap_len] = n;
                max_code = n as isize;
                self.depth[n] = 0;
            } else {
                tree[n].dl = 0;
            }
        }

        // A block needs at least two codes of non-zero frequency: the format
        // requires a distance code to exist, and one code alone would be
        // zero bits long.
        while self.heap_len < 2 {
            let new = if max_code < 2 {
                max_code += 1;
                max_code as usize
            } else {
                0
            };
            self.heap_len += 1;
            self.heap[self.heap_len] = new;
            tree[new].fc = 1;
            self.depth[new] = 0;
            self.opt_len = self.opt_len.wrapping_sub(1);
            if let Some(s) = stree {
                self.static_len = self.static_len.wrapping_sub(s[new].dl as u64);
            }
        }
        let max_code = max_code as usize;

        let mut n = self.heap_len / 2;
        while n >= 1 {
            self.pqdownheap(tree, n);
            n -= 1;
        }

        loop {
            let n = self.heap[SMALLEST];
            self.heap[SMALLEST] = self.heap[self.heap_len];
            self.heap_len -= 1;
            self.pqdownheap(tree, SMALLEST);
            let m = self.heap[SMALLEST];

            self.heap_max -= 1;
            self.heap[self.heap_max] = n;
            self.heap_max -= 1;
            self.heap[self.heap_max] = m;

            tree[node].fc = tree[n].fc.wrapping_add(tree[m].fc);
            self.depth[node] = self.depth[n].max(self.depth[m]) + 1;
            tree[n].dl = node as u16;
            tree[m].dl = node as u16;

            self.heap[SMALLEST] = node;
            node += 1;
            self.pqdownheap(tree, SMALLEST);
            if self.heap_len < 2 {
                break;
            }
        }
        self.heap_max -= 1;
        self.heap[self.heap_max] = self.heap[SMALLEST];

        self.gen_bitlen(tree, stree, desc, max_code);
        self.gen_codes(tree, max_code);
        max_code
    }
}

/// Block buffers, the three dynamic trees, the static trees, and the lookup
/// tables that map lengths and distances to codes.
struct Trees {
    out: BitWriter,
    tb: TreeBuild,
    dyn_ltree: Vec<Node>,
    dyn_dtree: Vec<Node>,
    bl_tree: Vec<Node>,
    static_ltree: Vec<Node>,
    static_dtree: Vec<Node>,
    l_max_code: usize,
    d_max_code: usize,
    bl_max_code: usize,

    /// Match length (minus `MIN_MATCH`) to length code.
    length_code: [u8; MAX_MATCH - MIN_MATCH + 1],
    /// Distance (minus one, divided by 128 above 256) to distance code.
    dist_code: [u8; 512],
    base_length: [u32; LENGTH_CODES],
    base_dist: [u32; D_CODES],

    /// The block's items: a literal, or a match length, per entry…
    l_buf: Vec<u8>,
    /// …with a distance for each match…
    d_buf: Vec<u16>,
    /// …and one bit per item saying which.
    flag_buf: Vec<u8>,
    last_lit: usize,
    last_dist: usize,
    last_flags: usize,
    flags: u8,
    flag_bit: u8,

    /// Bit length of everything emitted so far, as the block logic accounts
    /// for it; checked against what the writer actually sent.
    compressed_len: u64,
}

impl Trees {
    fn new() -> Self {
        let mut t = Trees {
            out: BitWriter::new(),
            tb: TreeBuild {
                bl_count: [0; MAX_BITS + 1],
                heap: [0; HEAP_SIZE],
                heap_len: 0,
                heap_max: 0,
                depth: [0; HEAP_SIZE],
                opt_len: 0,
                static_len: 0,
            },
            dyn_ltree: vec![Node::default(); HEAP_SIZE],
            dyn_dtree: vec![Node::default(); 2 * D_CODES + 1],
            bl_tree: vec![Node::default(); 2 * BL_CODES + 1],
            static_ltree: vec![Node::default(); L_CODES + 2],
            static_dtree: vec![Node::default(); D_CODES],
            l_max_code: 0,
            d_max_code: 0,
            bl_max_code: 0,
            length_code: [0; MAX_MATCH - MIN_MATCH + 1],
            dist_code: [0; 512],
            base_length: [0; LENGTH_CODES],
            base_dist: [0; D_CODES],
            l_buf: vec![0; LIT_BUFSIZE],
            d_buf: vec![0; DIST_BUFSIZE],
            flag_buf: vec![0; LIT_BUFSIZE / 8],
            last_lit: 0,
            last_dist: 0,
            last_flags: 0,
            flags: 0,
            flag_bit: 1,
            compressed_len: 0,
        };
        t.init_tables();
        t.init_block();
        t
    }

    fn init_tables(&mut self) {
        let mut length = 0usize;
        let mut code = 0usize;
        while code < LENGTH_CODES - 1 {
            self.base_length[code] = length as u32;
            for _ in 0..(1 << EXTRA_LBITS[code]) {
                self.length_code[length] = code as u8;
                length += 1;
            }
            code += 1;
        }
        debug_assert_eq!(length, 256);
        // Length 258 has its own code; prefer it to code 284 plus five bits.
        self.length_code[length - 1] = code as u8;

        let mut dist = 0usize;
        let mut code = 0usize;
        while code < 16 {
            self.base_dist[code] = dist as u32;
            for _ in 0..(1 << EXTRA_DBITS[code]) {
                self.dist_code[dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        debug_assert_eq!(dist, 256);
        dist >>= 7;
        while code < D_CODES {
            self.base_dist[code] = (dist << 7) as u32;
            for _ in 0..(1 << (EXTRA_DBITS[code] - 7)) {
                self.dist_code[256 + dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        debug_assert_eq!(dist, 256);

        self.tb.bl_count = [0; MAX_BITS + 1];
        let mut n = 0;
        while n <= 143 {
            self.static_ltree[n].dl = 8;
            self.tb.bl_count[8] += 1;
            n += 1;
        }
        while n <= 255 {
            self.static_ltree[n].dl = 9;
            self.tb.bl_count[9] += 1;
            n += 1;
        }
        while n <= 279 {
            self.static_ltree[n].dl = 7;
            self.tb.bl_count[7] += 1;
            n += 1;
        }
        while n <= 287 {
            self.static_ltree[n].dl = 8;
            self.tb.bl_count[8] += 1;
            n += 1;
        }
        // Codes 286 and 287 do not occur, but the canonical construction
        // needs them so that the longest code is all ones.
        self.tb.gen_codes(&mut self.static_ltree, L_CODES + 1);

        for n in 0..D_CODES {
            self.static_dtree[n].dl = 5;
            self.static_dtree[n].fc = bi_reverse(n as u32, 5) as u16;
        }
    }

    fn init_block(&mut self) {
        for n in self.dyn_ltree.iter_mut().take(L_CODES) {
            n.fc = 0;
        }
        for n in self.dyn_dtree.iter_mut().take(D_CODES) {
            n.fc = 0;
        }
        for n in self.bl_tree.iter_mut().take(BL_CODES) {
            n.fc = 0;
        }
        self.dyn_ltree[END_BLOCK].fc = 1;
        self.tb.opt_len = 0;
        self.tb.static_len = 0;
        self.last_lit = 0;
        self.last_dist = 0;
        self.last_flags = 0;
        self.flags = 0;
        self.flag_bit = 1;
    }

    fn d_code(&self, dist: usize) -> usize {
        (if dist < 256 { self.dist_code[dist] } else { self.dist_code[256 + (dist >> 7)] }) as usize
    }

    /// Record a literal (`dist == 0`, `lc` the byte) or a match (`dist` the
    /// distance, `lc` the length minus `MIN_MATCH`). Returns whether the
    /// block should be flushed now: the buffers are full, or — checked every
    /// 4096 items — the block has stopped compressing well enough to go on.
    /// `strstart` and `block_start` locate the block in the window for that
    /// second estimate.
    fn tally(&mut self, dist: usize, lc: usize, strstart: usize, block_start: i64) -> bool {
        self.l_buf[self.last_lit] = lc as u8;
        self.last_lit += 1;
        if dist == 0 {
            self.dyn_ltree[lc].fc = self.dyn_ltree[lc].fc.wrapping_add(1);
        } else {
            let dist = dist - 1;
            let lcode = self.length_code[lc] as usize + LITERALS + 1;
            self.dyn_ltree[lcode].fc = self.dyn_ltree[lcode].fc.wrapping_add(1);
            let dcode = self.d_code(dist);
            self.dyn_dtree[dcode].fc = self.dyn_dtree[dcode].fc.wrapping_add(1);
            self.d_buf[self.last_dist] = dist as u16;
            self.last_dist += 1;
            self.flags |= self.flag_bit;
        }
        self.flag_bit <<= 1;
        if self.last_lit & 7 == 0 {
            self.flag_buf[self.last_flags] = self.flags;
            self.last_flags += 1;
            self.flags = 0;
            self.flag_bit = 1;
        }

        if self.last_lit & 0xfff == 0 {
            let mut out_length = self.last_lit as u64 * 8;
            let in_length = (strstart as u64).wrapping_sub(block_start as u64);
            for dcode in 0..D_CODES {
                out_length += self.dyn_dtree[dcode].fc as u64 * (5 + EXTRA_DBITS[dcode] as u64);
            }
            out_length >>= 3;
            if self.last_dist < self.last_lit / 2 && out_length < in_length / 2 {
                return true;
            }
        }
        self.last_lit == LIT_BUFSIZE - 1 || self.last_dist == DIST_BUFSIZE
    }

    /// Count how the code lengths of `tree` will be transmitted, into the
    /// bit-length tree's frequencies.
    fn scan_tree(&mut self, which: Which, max_code: usize) {
        let tree = match which {
            Which::Lit => &mut self.dyn_ltree,
            Which::Dist => &mut self.dyn_dtree,
        };
        let mut prevlen: i32 = -1;
        let mut nextlen = tree[0].dl as i32;
        let mut count = 0;
        let mut max_count = 7;
        let mut min_count = 4;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        tree[max_code + 1].dl = 0xffff; // guard
        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = tree[n + 1].dl as i32;
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                self.bl_tree[curlen as usize].fc += count as u16;
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.bl_tree[curlen as usize].fc += 1;
                }
                self.bl_tree[REP_3_6].fc += 1;
            } else if count <= 10 {
                self.bl_tree[REPZ_3_10].fc += 1;
            } else {
                self.bl_tree[REPZ_11_138].fc += 1;
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn send_bl_code(&mut self, c: usize) {
        let node = self.bl_tree[c];
        self.out.send_bits(node.fc as u32, node.dl as u32);
    }

    /// Transmit the code lengths of `tree` with the bit-length codes.
    fn send_tree(&mut self, which: Which, max_code: usize) {
        let mut prevlen: i32 = -1;
        let tree_len = |t: &Trees, n: usize| -> i32 {
            match which {
                Which::Lit => t.dyn_ltree[n].dl as i32,
                Which::Dist => t.dyn_dtree[n].dl as i32,
            }
        };
        let mut nextlen = tree_len(self, 0);
        let mut count = 0;
        let mut max_count = 7;
        let mut min_count = 4;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = tree_len(self, n + 1);
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                loop {
                    self.send_bl_code(curlen as usize);
                    count -= 1;
                    if count == 0 {
                        break;
                    }
                }
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.send_bl_code(curlen as usize);
                    count -= 1;
                }
                debug_assert!((3..=6).contains(&count));
                self.send_bl_code(REP_3_6);
                self.out.send_bits(count as u32 - 3, 2);
            } else if count <= 10 {
                self.send_bl_code(REPZ_3_10);
                self.out.send_bits(count as u32 - 3, 3);
            } else {
                self.send_bl_code(REPZ_11_138);
                self.out.send_bits(count as u32 - 11, 7);
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    /// Build the bit-length tree and return the index in `BL_ORDER` of the
    /// last bit-length code that must be sent (at least 4 are).
    fn build_bl_tree(&mut self) -> usize {
        self.scan_tree(Which::Lit, self.l_max_code);
        self.scan_tree(Which::Dist, self.d_max_code);
        self.bl_max_code = self.tb.build_tree(&mut self.bl_tree, None, &BL_DESC);

        let mut max_blindex = BL_CODES - 1;
        while max_blindex >= 3 {
            if self.bl_tree[BL_ORDER[max_blindex] as usize].dl != 0 {
                break;
            }
            max_blindex -= 1;
        }
        self.tb.opt_len = self.tb.opt_len.wrapping_add(3 * (max_blindex as u64 + 1) + 5 + 5 + 4);
        max_blindex
    }

    fn send_all_trees(&mut self, lcodes: usize, dcodes: usize, blcodes: usize) {
        self.out.send_bits(lcodes as u32 - 257, 5);
        self.out.send_bits(dcodes as u32 - 1, 5);
        self.out.send_bits(blcodes as u32 - 4, 4);
        for rank in 0..blcodes {
            let len = self.bl_tree[BL_ORDER[rank] as usize].dl;
            self.out.send_bits(len as u32, 3);
        }
        self.send_tree(Which::Lit, lcodes - 1);
        self.send_tree(Which::Dist, dcodes - 1);
    }

    /// Emit the buffered items with the given trees, then the end-of-block
    /// code.
    fn compress_block(&mut self, stat: bool) {
        let mut lx = 0;
        let mut dx = 0;
        let mut fx = 0;
        let mut flag = 0u8;
        let code_of = |t: &Trees, c: usize, which: Which| -> (u32, u32) {
            let n = match (stat, which) {
                (true, Which::Lit) => t.static_ltree[c],
                (true, Which::Dist) => t.static_dtree[c],
                (false, Which::Lit) => t.dyn_ltree[c],
                (false, Which::Dist) => t.dyn_dtree[c],
            };
            (n.fc as u32, n.dl as u32)
        };
        if self.last_lit != 0 {
            loop {
                if lx & 7 == 0 {
                    flag = self.flag_buf[fx];
                    fx += 1;
                }
                let lc = self.l_buf[lx] as usize;
                lx += 1;
                if flag & 1 == 0 {
                    let (c, l) = code_of(self, lc, Which::Lit);
                    self.out.send_bits(c, l);
                } else {
                    let code = self.length_code[lc] as usize;
                    let (c, l) = code_of(self, code + LITERALS + 1, Which::Lit);
                    self.out.send_bits(c, l);
                    let extra = EXTRA_LBITS[code] as u32;
                    if extra != 0 {
                        let lc = lc as u32 - self.base_length[code];
                        self.out.send_bits(lc, extra);
                    }
                    let mut dist = self.d_buf[dx] as u32;
                    dx += 1;
                    let code = self.d_code(dist as usize);
                    let (c, l) = code_of(self, code, Which::Dist);
                    self.out.send_bits(c, l);
                    let extra = EXTRA_DBITS[code] as u32;
                    if extra != 0 {
                        dist -= self.base_dist[code];
                        self.out.send_bits(dist, extra);
                    }
                }
                flag >>= 1;
                if lx >= self.last_lit {
                    break;
                }
            }
        }
        let (c, l) = code_of(self, END_BLOCK, Which::Lit);
        self.out.send_bits(c, l);
    }

    /// Close the current block. `block` is the input it covers, when that
    /// input is still in the window (a block that began before the last
    /// slide cannot be sent stored); `eof` marks the last block.
    ///
    /// The block goes out stored when that is no longer than the best tree
    /// encoding (the four header bytes counted), with the static trees when
    /// they are no worse than dynamic ones, and with dynamic trees
    /// otherwise.
    fn flush_block(&mut self, block: Option<&[u8]>, stored_len: u64, eof: bool) {
        self.flag_buf[self.last_flags] = self.flags;

        self.l_max_code = self.tb.build_tree(&mut self.dyn_ltree, Some(&self.static_ltree), &L_DESC);
        self.d_max_code = self.tb.build_tree(&mut self.dyn_dtree, Some(&self.static_dtree), &D_DESC);
        let max_blindex = self.build_bl_tree();

        let mut opt_lenb = self.tb.opt_len.wrapping_add(3 + 7) >> 3;
        let static_lenb = self.tb.static_len.wrapping_add(3 + 7) >> 3;
        if static_lenb <= opt_lenb {
            opt_lenb = static_lenb;
        }
        let eof_bit = eof as u32;

        if let Some(buf) = block.filter(|_| stored_len + 4 <= opt_lenb) {
            self.out.send_bits((STORED_BLOCK << 1) + eof_bit, 3);
            self.compressed_len = (self.compressed_len + 3 + 7) & !7;
            self.compressed_len += (stored_len + 4) << 3;
            self.out.copy_block(buf, true);
        } else if static_lenb == opt_lenb {
            self.out.send_bits((STATIC_TREES << 1) + eof_bit, 3);
            self.compress_block(true);
            self.compressed_len = self.compressed_len.wrapping_add(3 + self.tb.static_len);
        } else {
            self.out.send_bits((DYN_TREES << 1) + eof_bit, 3);
            self.send_all_trees(self.l_max_code + 1, self.d_max_code + 1, max_blindex + 1);
            self.compress_block(false);
            self.compressed_len = self.compressed_len.wrapping_add(3 + self.tb.opt_len);
        }
        debug_assert_eq!(self.compressed_len, self.out.bits_sent, "block length bookkeeping");
        self.init_block();

        if eof {
            self.out.bi_windup();
            self.compressed_len += 7;
        }
    }
}

#[derive(Clone, Copy)]
enum Which {
    Lit,
    Dist,
}

// ---------------------------------------------------------------------------
// Sliding window and match finding
// ---------------------------------------------------------------------------

struct Deflater<'a> {
    input: &'a [u8],
    /// Next input byte not yet copied into the window.
    in_pos: usize,

    window: Vec<u8>,
    /// Per window slot, the previous position with the same hash.
    prev: Vec<u16>,
    /// Per hash, the most recent position inserted.
    head: Vec<u16>,
    ins_h: usize,

    /// Start of the string being matched.
    strstart: usize,
    /// Start of the best match found by the last search.
    match_start: usize,
    /// Length of the best match at the previous position.
    prev_length: usize,
    /// Valid bytes ahead of `strstart`.
    lookahead: usize,
    eofile: bool,
    /// Window offset of the current block's first byte; negative once the
    /// window has slid past it.
    block_start: i64,

    trees: Trees,
}

impl<'a> Deflater<'a> {
    fn new(input: &'a [u8]) -> Self {
        Deflater {
            input,
            in_pos: 0,
            window: vec![0; WINDOW_SIZE],
            prev: vec![0; WSIZE],
            head: vec![0; HASH_SIZE],
            ins_h: 0,
            strstart: 0,
            match_start: 0,
            prev_length: 0,
            lookahead: 0,
            eofile: false,
            block_start: 0,
            trees: Trees::new(),
        }
    }

    /// Copy up to `size` input bytes into the window at `at`; 0 at the end
    /// of the input.
    fn read_buf(&mut self, at: usize, size: usize) -> usize {
        let n = size.min(self.input.len() - self.in_pos);
        self.window[at..at + n].copy_from_slice(&self.input[self.in_pos..self.in_pos + n]);
        self.in_pos += n;
        n
    }

    fn update_hash(&mut self, c: u8) {
        self.ins_h = ((self.ins_h << H_SHIFT) ^ c as usize) & HASH_MASK;
    }

    /// Insert the string at `s` into the dictionary and return the previous
    /// head of its hash chain.
    fn insert_string(&mut self, s: usize) -> usize {
        self.update_hash(self.window[s + MIN_MATCH - 1]);
        let head = self.head[self.ins_h];
        self.prev[s & WMASK] = head;
        self.head[self.ins_h] = s as u16;
        head as usize
    }

    fn lm_init(&mut self) {
        self.head.fill(NIL);
        self.strstart = 0;
        self.block_start = 0;
        self.lookahead = self.read_buf(0, WINDOW_SIZE);
        if self.lookahead == 0 {
            self.eofile = true;
            return;
        }
        self.eofile = false;
        while self.lookahead < MIN_LOOKAHEAD && !self.eofile {
            self.fill_window();
        }
        self.ins_h = 0;
        for j in 0..MIN_MATCH - 1 {
            self.update_hash(self.window[j]);
        }
    }

    /// Find the longest match for the string at `strstart` along the hash
    /// chain from `cur_match`, longer than `prev_length`; sets `match_start`
    /// and returns its length (or `prev_length` when nothing longer exists).
    fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let mut chain_length = MAX_CHAIN;
        let strstart = self.strstart;
        let mut best_len = self.prev_length;
        let limit = if strstart > MAX_DIST { strstart - MAX_DIST } else { NIL as usize };
        let w = &self.window;
        let strend = strstart + MAX_MATCH;
        let mut scan_end1 = w[strstart + best_len - 1];
        let mut scan_end = w[strstart + best_len];

        if self.prev_length >= GOOD_LENGTH {
            chain_length >>= 2;
        }
        debug_assert!(strstart <= WINDOW_SIZE - MIN_LOOKAHEAD, "insufficient lookahead");

        loop {
            debug_assert!(cur_match < strstart, "no future");
            let m = cur_match;
            if w[m + best_len] == scan_end
                && w[m + best_len - 1] == scan_end1
                && w[m] == w[strstart]
                && w[m + 1] == w[strstart + 1]
            {
                // The third byte is known equal: the hashes are equal and so
                // are the two bytes before it. Compare in runs of eight, so
                // the lookahead check lands exactly on `MAX_MATCH`.
                let mut scan = strstart + 2;
                let mut mat = m + 2;
                loop {
                    let mut all = true;
                    for _ in 0..8 {
                        scan += 1;
                        mat += 1;
                        if w[scan] != w[mat] {
                            all = false;
                            break;
                        }
                    }
                    if !all || scan >= strend {
                        break;
                    }
                }
                let len = MAX_MATCH - (strend - scan);
                if len > best_len {
                    self.match_start = m;
                    best_len = len;
                    if len >= NICE_LENGTH {
                        break;
                    }
                    scan_end1 = w[strstart + best_len - 1];
                    scan_end = w[strstart + best_len];
                }
            }
            cur_match = self.prev[cur_match & WMASK] as usize;
            if cur_match <= limit {
                break;
            }
            chain_length -= 1;
            if chain_length == 0 {
                break;
            }
        }
        best_len
    }

    /// Top the window up from the input; when the upper half is full, slide
    /// it down first and rebase every position by `WSIZE`.
    fn fill_window(&mut self) {
        let mut more = WINDOW_SIZE - self.lookahead - self.strstart;
        if self.strstart >= WSIZE + MAX_DIST {
            self.window.copy_within(WSIZE..WINDOW_SIZE, 0);
            self.match_start = self.match_start.wrapping_sub(WSIZE);
            self.strstart -= WSIZE;
            self.block_start -= WSIZE as i64;
            for h in self.head.iter_mut() {
                *h = if *h as usize >= WSIZE { *h - WSIZE as u16 } else { NIL };
            }
            for p in self.prev.iter_mut() {
                *p = if *p as usize >= WSIZE { *p - WSIZE as u16 } else { NIL };
            }
            more += WSIZE;
        }
        if !self.eofile {
            let at = self.strstart + self.lookahead;
            let n = self.read_buf(at, more);
            if n == 0 {
                self.eofile = true;
                // The two bytes after the input would otherwise enter the
                // hash as whatever the window held before.
                self.window[at..at + MIN_MATCH - 1].fill(0);
            } else {
                self.lookahead += n;
            }
        }
    }

    fn flush_block(&mut self, eof: bool) {
        let stored_len = (self.strstart as i64 - self.block_start) as u64;
        let block = if self.block_start >= 0 {
            let start = self.block_start as usize;
            Some(&self.window[start..self.strstart])
        } else {
            None
        };
        self.trees.flush_block(block, stored_len, eof);
    }

    fn tally(&mut self, dist: usize, lc: usize) -> bool {
        self.trees.tally(dist, lc, self.strstart, self.block_start)
    }

    /// Lazy-match deflate: at each position find the longest match, but emit
    /// the previous position's match only if the current one is no better;
    /// otherwise the previous byte goes out as a literal and the decision
    /// moves on.
    fn deflate(&mut self) {
        self.lm_init();
        let mut match_length = MIN_MATCH - 1;
        let mut prev_match: usize;
        let mut match_available = false;

        while self.lookahead != 0 {
            let hash_head = self.insert_string(self.strstart);

            self.prev_length = match_length;
            prev_match = self.match_start;
            match_length = MIN_MATCH - 1;

            if hash_head != NIL as usize
                && self.prev_length < MAX_LAZY
                && self.strstart.wrapping_sub(hash_head) <= MAX_DIST
                && self.strstart <= WINDOW_SIZE - MIN_LOOKAHEAD
            {
                match_length = self.longest_match(hash_head);
                if match_length > self.lookahead {
                    match_length = self.lookahead;
                }
                if match_length == MIN_MATCH && self.strstart.wrapping_sub(self.match_start) > TOO_FAR {
                    match_length -= 1;
                }
            }

            if self.prev_length >= MIN_MATCH && match_length <= self.prev_length {
                let flush = self.tally(self.strstart - 1 - prev_match, self.prev_length - MIN_MATCH);
                self.lookahead -= self.prev_length - 1;
                self.prev_length -= 2;
                loop {
                    self.strstart += 1;
                    self.insert_string(self.strstart);
                    self.prev_length -= 1;
                    if self.prev_length == 0 {
                        break;
                    }
                }
                match_available = false;
                match_length = MIN_MATCH - 1;
                self.strstart += 1;
                if flush {
                    self.flush_block(false);
                    self.block_start = self.strstart as i64;
                }
            } else if match_available {
                let flush = self.tally(0, self.window[self.strstart - 1] as usize);
                if flush {
                    self.flush_block(false);
                    self.block_start = self.strstart as i64;
                }
                self.strstart += 1;
                self.lookahead -= 1;
            } else {
                match_available = true;
                self.strstart += 1;
                self.lookahead -= 1;
            }

            while self.lookahead < MIN_LOOKAHEAD && !self.eofile {
                self.fill_window();
            }
        }
        if match_available {
            self.tally(0, self.window[self.strstart - 1] as usize);
        }
        self.flush_block(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    /// `gzip -6 -n -c` over `input`, or None (with a note) when no `gzip`
    /// is on PATH.
    fn reference(input: &[u8]) -> Option<Vec<u8>> {
        let mut child = match Command::new("gzip")
            .args(["-6", "-n", "-c"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("note: gzip not runnable ({e}); skipping reference comparison");
                return None;
            }
        };
        let mut stdin = child.stdin.take().unwrap();
        let data = input.to_vec();
        let writer = std::thread::spawn(move || stdin.write_all(&data));
        let mut out = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
        writer.join().unwrap().unwrap();
        assert!(child.wait().unwrap().success());
        Some(out)
    }

    fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
        a.iter().zip(b).position(|(x, y)| x != y).or(if a.len() != b.len() { Some(a.len().min(b.len())) } else { None })
    }

    fn check(name: &str, input: &[u8]) {
        let ours = gzip_member(input, 0, None);
        // The stream must at least round-trip.
        let mut back = Vec::new();
        flate2::read::GzDecoder::new(&ours[..]).read_to_end(&mut back).unwrap();
        assert!(back == input, "{name}: round trip lost data");
        if let Some(theirs) = reference(input) {
            if let Some(off) = first_difference(&ours, &theirs) {
                panic!(
                    "{name}: first difference at byte {off} (ours {} bytes, gzip {} bytes): ours {:02x?} gzip {:02x?}",
                    ours.len(),
                    theirs.len(),
                    &ours[off.saturating_sub(4)..(off + 8).min(ours.len())],
                    &theirs[off.saturating_sub(4)..(off + 8).min(theirs.len())],
                );
            }
        }
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    #[test]
    fn gzip_matches_reference_on_empty_input() {
        check("empty", b"");
    }

    #[test]
    fn gzip_matches_reference_on_a_few_bytes() {
        check("few bytes", b"hello, hello, hello world\n");
        check("one byte", b"x");
        check("two bytes", b"ab");
    }

    #[test]
    fn gzip_matches_reference_on_repetitive_input() {
        let mut v = Vec::with_capacity(100 * 1024);
        while v.len() < 60 * 1024 {
            v.extend_from_slice(b"abcabcabd the quick brown fox ");
        }
        v.resize(80 * 1024, 0);
        while v.len() < 100 * 1024 {
            v.extend_from_slice(b"zzzzzzzzzzzzzzzzzzzzzy");
        }
        check("repetitive 100K", &v);
    }

    #[test]
    fn gzip_matches_reference_on_random_input() {
        let mut g = Lcg(42);
        let v: Vec<u8> = (0..300 * 1024).map(|_| g.next() as u8).collect();
        check("random 300K", &v);
    }

    /// Text-like data whose repeats straddle the 32K and 64K window
    /// boundaries, so matches, window slides and block flushes interleave.
    fn mixed(len: usize) -> Vec<u8> {
        const WORDS: [&str; 24] = [
            "ontology", "class", "subClassOf", "part_of", "develops_from", "anatomical", "entity",
            "UBERON", "CL", "label", "definition", "synonym", "xref", "comment", "structure",
            "tissue", "organ", "cell", "system", "region", "layer", "membrane", "nucleus", "axis",
        ];
        let mut g = Lcg(7);
        let mut v = Vec::with_capacity(len);
        while v.len() < len {
            if v.len() > 40_000 && g.next() % 11 == 0 {
                // Copy a chunk from a while back; long enough to cross a
                // 32K/64K boundary when it starts near one.
                let back = 10_000 + (g.next() as usize % 30_000);
                let n = 1_000 + (g.next() as usize % 20_000);
                let start = v.len() - back;
                for i in 0..n {
                    let b = v[start + i];
                    v.push(b);
                }
                continue;
            }
            let w = WORDS[g.next() as usize % WORDS.len()];
            v.extend_from_slice(w.as_bytes());
            v.push(if g.next() % 9 == 0 { b'\n' } else { b' ' });
            if g.next() % 5 == 0 {
                v.extend_from_slice(format!("{:08x}", g.next()).as_bytes());
            }
        }
        v.truncate(len);
        v
    }

    #[test]
    fn gzip_matches_reference_on_mixed_input() {
        check("mixed 2M", &mixed(2 * 1024 * 1024));
    }

    fn json_like(len: usize) -> Vec<u8> {
        let mut g = Lcg(99);
        let mut v = Vec::with_capacity(len);
        v.extend_from_slice(b"{\"graphs\":[{\"nodes\":[");
        let mut i = 0u32;
        while v.len() < len {
            let line = format!(
                "{{\"id\":\"http://purl.obolibrary.org/obo/UBERON_{:07}\",\"lbl\":\"term {} of kind {}\",\"type\":\"CLASS\",\"meta\":{{\"definition\":{{\"val\":\"A {} that is part of some {}.\",\"xrefs\":[\"ISBN:{:010}\"]}},\"synonyms\":[{{\"pred\":\"hasExactSynonym\",\"val\":\"syn {}\"}}]}}}},\n",
                i,
                g.next() % 5000,
                g.next() % 37,
                ["bone", "nerve", "vessel", "gland", "muscle"][g.next() as usize % 5],
                ["head", "limb", "trunk", "tail"][g.next() as usize % 4],
                g.next(),
                g.next() % 100,
            );
            v.extend_from_slice(line.as_bytes());
            i += 1;
        }
        v.truncate(len);
        v
    }

    #[test]
    fn gzip_matches_reference_on_json_like_input() {
        check("json 3M", &json_like(3 * 1024 * 1024));
    }

    #[test]
    fn crc32_is_the_gzip_crc() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    /// The 55 MB uberon.json: the member body after the 22-byte header
    /// (10 bytes plus `uberon.json\0`) has a known md5. Run with
    /// `cargo test --release -- --ignored gzip`.
    #[test]
    #[ignore]
    fn gzip_matches_reference_on_uberon_json() {
        let path = "/data2/ontologies/uberon-ref2/src/ontology/uberon.json";
        let input = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("note: {path} unreadable ({e}); skipping");
                return;
            }
        };
        let ours = gzip_member(&input, 0, Some(b"uberon.json"));
        assert_eq!(ours.len(), 4_228_230, "total size with the name in the header");
        let mut child = Command::new("md5sum").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&ours[22..]).unwrap();
        let mut sum = String::new();
        child.stdout.take().unwrap().read_to_string(&mut sum).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(&sum[..32], "831d7009b68d5fb1c2d0430a0b92dd5c");
        if let Some(theirs) = reference(&input) {
            assert_eq!(first_difference(&ours[22..], &theirs[10..]), None);
        }
    }
}
