//! Flat, pointer-free Aho-Corasick automaton backing the term tagger.
//!
//! The tagger DB is an interchange format in both directions: owlmake loads published
//! `text_tagger_db.bin.gz` files, and the DBs it writes are consumed by OLS. The
//! on-disk form is a hand-rolled flat buffer (no serde/bincode), all little-endian,
//! with **no magic or version field** — nothing in a DB states which layout it is in,
//! so there is no check that can detect a mismatch. Do not "improve" the
//! serialization: any change to it breaks both directions.
//!
//! Layout (see `NerAcBuilder::build`): 24-byte header (6×u32) + state table
//! (18-byte records) + transition table (5-byte records, sorted by byte within each
//! state) + pattern-lengths (u32) + values-index (u32 off,len) + values-data blob.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};

/// Record separator: delimits fields within one term record
/// (`label ␞ iri ␞ ontology_id ␞ string_type ␞ source ␞ categories ␞ is_obsolete`).
pub const RECORD_SEP: char = '\x1E';

/// Unit separator: delimits multiple term records stored against the same key.
pub const UNIT_SEP: char = '\x1F';

// ============================================================================
// Builder
// ============================================================================

struct BuilderState {
    goto: HashMap<u8, u32>,
    value_idx: u32, // u32::MAX = none
}

pub struct NerAcBuilder {
    states: Vec<BuilderState>,
    /// (lowercased key bytes, value string)
    patterns: Vec<(Vec<u8>, String)>,
    entry_count: usize,
    key_set: HashMap<Vec<u8>, usize>, // dedup: lowered key -> index in `patterns`
}

impl Default for NerAcBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl NerAcBuilder {
    pub fn new() -> Self {
        NerAcBuilder {
            states: vec![BuilderState {
                goto: HashMap::new(),
                value_idx: u32::MAX,
            }],
            patterns: Vec::new(),
            entry_count: 0,
            key_set: HashMap::new(),
        }
    }

    /// Add `key` (matched case-insensitively) → `value`. If the key already exists,
    /// the new value is appended (separated by `UNIT_SEP`) so a single key can
    /// resolve to multiple terms.
    pub fn add_entry(&mut self, key: &str, value: &str) {
        if key.is_empty() {
            return;
        }
        let lower: Vec<u8> = key.bytes().map(|b| b.to_ascii_lowercase()).collect();

        if let Some(&idx) = self.key_set.get(&lower) {
            self.patterns[idx].1.push(UNIT_SEP);
            self.patterns[idx].1.push_str(value);
            return;
        }

        let pat_idx = self.patterns.len();
        self.key_set.insert(lower.clone(), pat_idx);
        self.patterns.push((lower, value.to_string()));
        self.entry_count += 1;
    }

    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Consume the builder and produce a flat, pointer-free `NerAc`.
    pub fn build(mut self) -> NerAc {
        // ---- Phase 1: build goto trie ----
        for pat_idx in 0..self.patterns.len() {
            let key = self.patterns[pat_idx].0.clone();
            let mut cur: u32 = 0;
            for &b in key.iter() {
                cur = match self.states[cur as usize].goto.get(&b) {
                    Some(&next) => next,
                    None => {
                        let new_id = self.states.len() as u32;
                        self.states.push(BuilderState {
                            goto: HashMap::new(),
                            value_idx: u32::MAX,
                        });
                        self.states[cur as usize].goto.insert(b, new_id);
                        new_id
                    }
                };
            }
            self.states[cur as usize].value_idx = pat_idx as u32;
        }

        let ns = self.states.len() as u32;

        // ---- Phase 2: failure + output links (BFS) ----
        let mut failure: Vec<u32> = vec![0; ns as usize];
        let mut output_link: Vec<u32> = vec![u32::MAX; ns as usize];
        let mut queue: VecDeque<u32> = VecDeque::new();

        for &child in self.states[0].goto.values() {
            failure[child as usize] = 0;
            queue.push_back(child);
        }

        while let Some(u) = queue.pop_front() {
            let children: Vec<(u8, u32)> = self.states[u as usize]
                .goto
                .iter()
                .map(|(&b, &s)| (b, s))
                .collect();
            for (b, v) in children {
                queue.push_back(v);
                let mut f = failure[u as usize];
                loop {
                    if let Some(&t) = self.states[f as usize].goto.get(&b) {
                        failure[v as usize] = t;
                        break;
                    }
                    if f == 0 {
                        failure[v as usize] = 0;
                        break;
                    }
                    f = failure[f as usize];
                }
                let fl = failure[v as usize];
                output_link[v as usize] = if self.states[fl as usize].value_idx != u32::MAX {
                    fl
                } else {
                    output_link[fl as usize]
                };
            }
        }

        // ---- Phase 3: serialise into a flat buffer ----
        // Header (24 bytes): num_states, num_patterns, trans_tbl_off,
        // pat_lengths_off, values_idx_off, values_data_off (all u32 LE).
        // State record (18 bytes): failure:u32, output_link:u32, value_idx:u32,
        // trans_count:u16, trans_offset:u32 (record index into transition table).
        // Transition record (5 bytes): byte:u8, target:u32 (sorted by byte / state).
        // Pattern-lengths: u32 each. Values-index: (data_off:u32, data_len:u32).
        // Values-data: raw concatenated bytes.
        const HEADER_SIZE: usize = 24;
        const STATE_REC: usize = 18;

        let np = self.patterns.len() as u32;

        // Sorted transitions per state
        let mut state_trans: Vec<Vec<(u8, u32)>> = Vec::with_capacity(ns as usize);
        for s in &self.states {
            let mut t: Vec<(u8, u32)> = s.goto.iter().map(|(&b, &id)| (b, id)).collect();
            t.sort_by_key(|&(b, _)| b);
            state_trans.push(t);
        }
        let total_trans: usize = state_trans.iter().map(|v| v.len()).sum();

        let trans_tbl_off = (HEADER_SIZE + (ns as usize) * STATE_REC) as u32;
        let pat_lengths_off = trans_tbl_off + (total_trans as u32) * 5;
        let values_idx_off = pat_lengths_off + np * 4;

        // values data
        let mut values_data: Vec<u8> = Vec::new();
        let mut values_index: Vec<(u32, u32)> = Vec::with_capacity(np as usize);
        for (_, val) in &self.patterns {
            let off = values_data.len() as u32;
            let b = val.as_bytes();
            values_data.extend_from_slice(b);
            values_index.push((off, b.len() as u32));
        }

        let values_data_off = values_idx_off + np * 8;
        let total = values_data_off as usize + values_data.len();

        let mut buf: Vec<u8> = Vec::with_capacity(total);

        // -- header --
        buf.extend(ns.to_le_bytes());
        buf.extend(np.to_le_bytes());
        buf.extend(trans_tbl_off.to_le_bytes());
        buf.extend(pat_lengths_off.to_le_bytes());
        buf.extend(values_idx_off.to_le_bytes());
        buf.extend(values_data_off.to_le_bytes());

        // -- state table --
        let mut t_off: u32 = 0;
        for i in 0..ns as usize {
            buf.extend(failure[i].to_le_bytes());
            buf.extend(output_link[i].to_le_bytes());
            buf.extend(self.states[i].value_idx.to_le_bytes());
            buf.extend((state_trans[i].len() as u16).to_le_bytes());
            buf.extend(t_off.to_le_bytes());
            t_off += state_trans[i].len() as u32;
        }

        // -- transition table --
        for tl in &state_trans {
            for &(b, tgt) in tl {
                buf.push(b);
                buf.extend(tgt.to_le_bytes());
            }
        }

        // -- pattern lengths --
        for (key, _) in &self.patterns {
            buf.extend((key.len() as u32).to_le_bytes());
        }

        // -- values index --
        for &(off, len) in &values_index {
            buf.extend(off.to_le_bytes());
            buf.extend(len.to_le_bytes());
        }

        // -- values data --
        buf.extend(&values_data);

        buf.shrink_to_fit();
        NerAc { buf }
    }
}

// ============================================================================
// Runtime: flat, pointer-free Aho-Corasick automaton
// ============================================================================

pub struct NerMatch {
    pub start: usize,
    pub end: usize,
    pub value: String,
}

pub struct NerAc {
    pub buf: Vec<u8>,
}

const HEADER_SIZE: usize = 24;
const STATE_REC: usize = 18;
const TRANS_REC: usize = 5;

impl NerAc {
    /// Adopt `buf` as a tagger DB, checking that it really is one.
    ///
    /// The runtime below indexes the buffer directly and unwraps every read, and
    /// it is entitled to: nothing reaches it that has not been through here. The
    /// format carries no magic number and no version, so a file that is merely
    /// not a tagger DB — a truncated download, a text file, a DB from an
    /// incompatible build — arrives as plausible little-endian integers, and
    /// every one of them is an offset or a state id the automaton would follow.
    /// Checked here they are a named error; unchecked they are an out-of-bounds
    /// index, which for a long-lived tagging service means one bad DB ends the
    /// process.
    ///
    /// What is verified is everything the runtime trusts: that the sections tile
    /// the buffer exactly as `NerAcBuilder::build` lays them out, that every
    /// state, pattern and transition reference is in range, that the transitions
    /// of each state are sorted (`goto` binary-searches them), that each value
    /// span lies inside the values blob, and that failure and output links climb
    /// strictly towards the root — which is what makes the walks in `emit` and
    /// `find_all_matches` terminate. Pattern lengths are checked against the trie
    /// depth of the state that emits them, so a match can never start before the
    /// text does.
    pub fn from_buf(buf: Vec<u8>) -> anyhow::Result<Self> {
        use anyhow::{bail, Context};

        fn le_u32(buf: &[u8], o: usize) -> u32 {
            u32::from_le_bytes(buf[o..o + 4].try_into().expect("checked 4-byte window"))
        }
        let u32_at = |o: usize| le_u32(&buf, o);

        if buf.len() < HEADER_SIZE {
            bail!(
                "not a tagger DB: {} bytes, less than the {HEADER_SIZE}-byte header",
                buf.len()
            );
        }
        let num_states = u32_at(0);
        let num_patterns = u32_at(4);
        let trans_tbl_off = u32_at(8) as usize;
        let pat_lengths_off = u32_at(12) as usize;
        let values_idx_off = u32_at(16) as usize;
        let values_data_off = u32_at(20) as usize;

        if num_states == 0 {
            bail!("not a tagger DB: it declares no states (every DB has at least a root)");
        }
        let ns = num_states as usize;
        let np = num_patterns as usize;

        // The sections tile the buffer in order, each starting exactly where the
        // previous one ends, so every boundary is derivable and must agree with
        // what the header claims.
        let want_trans = HEADER_SIZE
            .checked_add(ns.checked_mul(STATE_REC).context("state table size overflows")?)
            .context("state table end overflows")?;
        if trans_tbl_off != want_trans {
            bail!(
                "not a tagger DB: {num_states} states put the transition table at {want_trans}, \
                 but the header says {trans_tbl_off}"
            );
        }
        if pat_lengths_off < trans_tbl_off || (pat_lengths_off - trans_tbl_off) % TRANS_REC != 0 {
            bail!(
                "not a tagger DB: the transition table spans {trans_tbl_off}..{pat_lengths_off}, \
                 which is not a whole number of {TRANS_REC}-byte records"
            );
        }
        let total_trans = (pat_lengths_off - trans_tbl_off) / TRANS_REC;
        let want_values_idx =
            pat_lengths_off.checked_add(np.checked_mul(4).context("pattern lengths overflow")?);
        if want_values_idx != Some(values_idx_off) {
            bail!(
                "not a tagger DB: {num_patterns} patterns put the values index at {:?}, \
                 but the header says {values_idx_off}",
                want_values_idx
            );
        }
        let want_values_data =
            values_idx_off.checked_add(np.checked_mul(8).context("values index overflows")?);
        if want_values_data != Some(values_data_off) {
            bail!(
                "not a tagger DB: {num_patterns} patterns put the values data at {:?}, \
                 but the header says {values_data_off}",
                want_values_data
            );
        }
        if values_data_off > buf.len() {
            bail!(
                "truncated tagger DB: the values data starts at {values_data_off}, past the end \
                 of {} bytes",
                buf.len()
            );
        }
        let values_data_len = buf.len() - values_data_off;

        // Every state's references, and its slice of the transition table.
        for s in 0..ns {
            let o = HEADER_SIZE + s * STATE_REC;
            let failure = u32_at(o);
            let output_link = u32_at(o + 4);
            let value_idx = u32_at(o + 8);
            let trans_count = u16::from_le_bytes(
                buf[o + 12..o + 14].try_into().expect("checked 2-byte window"),
            ) as usize;
            let trans_offset = u32_at(o + 14) as usize;
            if failure as usize >= ns {
                bail!("corrupt tagger DB: state {s} fails to state {failure}, of {num_states}");
            }
            if output_link != u32::MAX && output_link as usize >= ns {
                bail!("corrupt tagger DB: state {s} links out to state {output_link}, of {num_states}");
            }
            if value_idx != u32::MAX && value_idx as usize >= np {
                bail!("corrupt tagger DB: state {s} carries pattern {value_idx}, of {num_patterns}");
            }
            let end = trans_offset
                .checked_add(trans_count)
                .context("a state's transition range overflows")?;
            if end > total_trans {
                bail!(
                    "corrupt tagger DB: state {s} claims transitions {trans_offset}..{end}, \
                     of {total_trans}"
                );
            }
            // `goto` binary-searches this run, so the keys must be sorted and
            // distinct; unsorted keys would not crash, they would simply fail to
            // find terms that are in the DB.
            let base = trans_tbl_off + trans_offset * TRANS_REC;
            for t in 0..trans_count {
                let toff = base + t * TRANS_REC;
                let target = u32_at(toff + 1);
                if target as usize >= ns {
                    bail!(
                        "corrupt tagger DB: state {s} moves to state {target}, of {num_states}"
                    );
                }
                if t > 0 && buf[toff] <= buf[toff - TRANS_REC] {
                    bail!("corrupt tagger DB: state {s}'s transitions are not sorted by byte");
                }
            }
        }

        // Every value span lies inside the values blob.
        for p in 0..np {
            let vi = values_idx_off + p * 8;
            let d_off = u32_at(vi) as usize;
            let d_len = u32_at(vi + 4) as usize;
            let end = d_off.checked_add(d_len).context("a value span overflows")?;
            if end > values_data_len {
                bail!(
                    "corrupt tagger DB: pattern {p}'s value spans {d_off}..{end} of a \
                     {values_data_len}-byte values blob"
                );
            }
        }

        let ac = NerAc { buf };

        // Trie depth, by breadth-first walk from the root. It settles three things
        // at once: every state is reachable (the builder produces no orphans), a
        // pattern's recorded length is the depth at which it is emitted (so
        // `emit` can subtract it from the end position without going behind the
        // start of the text), and failure/output links climb strictly towards the
        // root — the property that makes the failure walk in `find_all_matches`
        // and the output-link walk in `emit` terminate rather than cycle.
        let mut depth: Vec<Option<u32>> = vec![None; ns];
        depth[0] = Some(0);
        let mut queue: VecDeque<u32> = VecDeque::new();
        queue.push_back(0);
        while let Some(s) = queue.pop_front() {
            let d = depth[s as usize].expect("queued only when known");
            let count = ac.state_trans_count(s) as usize;
            let base = trans_tbl_off + ac.state_trans_offset(s) as usize * TRANS_REC;
            for t in 0..count {
                let target = le_u32(&ac.buf, base + t * TRANS_REC + 1);
                if depth[target as usize].is_none() {
                    depth[target as usize] = Some(d + 1);
                    queue.push_back(target);
                }
            }
        }
        for s in 0..ns {
            let Some(d) = depth[s] else {
                bail!("corrupt tagger DB: state {s} is not reachable from the root");
            };
            let s32 = s as u32;
            if s32 != 0 {
                let f = ac.state_failure(s32);
                let fd = depth[f as usize].expect("all states reachable by here");
                if fd >= d {
                    bail!(
                        "corrupt tagger DB: state {s} at depth {d} fails to state {f} at depth \
                         {fd}, which would not terminate"
                    );
                }
            }
            let ol = ac.state_output_link(s32);
            if ol != u32::MAX {
                let od = depth[ol as usize].expect("all states reachable by here");
                if od >= d {
                    bail!(
                        "corrupt tagger DB: state {s} at depth {d} links out to state {ol} at \
                         depth {od}, which would not terminate"
                    );
                }
            }
            let vi = ac.state_value_idx(s32);
            if vi != u32::MAX {
                let plen = ac.pattern_length(vi);
                if plen != d {
                    bail!(
                        "corrupt tagger DB: pattern {vi} is {plen} bytes long but is emitted at \
                         depth {d}"
                    );
                }
            }
        }

        Ok(ac)
    }

    // -- header accessors --

    #[inline(always)]
    fn num_states(&self) -> u32 {
        u32::from_le_bytes(self.buf[0..4].try_into().unwrap())
    }
    #[inline(always)]
    fn num_patterns(&self) -> u32 {
        u32::from_le_bytes(self.buf[4..8].try_into().unwrap())
    }
    #[inline(always)]
    fn trans_tbl_off(&self) -> u32 {
        u32::from_le_bytes(self.buf[8..12].try_into().unwrap())
    }
    #[inline(always)]
    fn pat_lengths_off(&self) -> u32 {
        u32::from_le_bytes(self.buf[12..16].try_into().unwrap())
    }
    #[inline(always)]
    fn values_idx_off(&self) -> u32 {
        u32::from_le_bytes(self.buf[16..20].try_into().unwrap())
    }
    #[inline(always)]
    fn values_data_off(&self) -> u32 {
        u32::from_le_bytes(self.buf[20..24].try_into().unwrap())
    }

    // -- state accessors --

    #[inline(always)]
    fn state_off(s: u32) -> usize {
        HEADER_SIZE + s as usize * STATE_REC
    }
    #[inline(always)]
    fn state_failure(&self, s: u32) -> u32 {
        let o = Self::state_off(s);
        u32::from_le_bytes(self.buf[o..o + 4].try_into().unwrap())
    }
    #[inline(always)]
    fn state_output_link(&self, s: u32) -> u32 {
        let o = Self::state_off(s) + 4;
        u32::from_le_bytes(self.buf[o..o + 4].try_into().unwrap())
    }
    #[inline(always)]
    fn state_value_idx(&self, s: u32) -> u32 {
        let o = Self::state_off(s) + 8;
        u32::from_le_bytes(self.buf[o..o + 4].try_into().unwrap())
    }
    #[inline(always)]
    fn state_trans_count(&self, s: u32) -> u16 {
        let o = Self::state_off(s) + 12;
        u16::from_le_bytes(self.buf[o..o + 2].try_into().unwrap())
    }
    #[inline(always)]
    fn state_trans_offset(&self, s: u32) -> u32 {
        let o = Self::state_off(s) + 14;
        u32::from_le_bytes(self.buf[o..o + 4].try_into().unwrap())
    }

    // -- transition lookup (binary search over sorted byte keys) --

    #[inline(always)]
    fn goto(&self, state: u32, byte: u8) -> Option<u32> {
        let count = self.state_trans_count(state) as usize;
        if count == 0 {
            return None;
        }
        let base =
            self.trans_tbl_off() as usize + self.state_trans_offset(state) as usize * TRANS_REC;

        let mut lo: usize = 0;
        let mut hi: usize = count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = base + mid * TRANS_REC;
            let b = self.buf[off];
            if b == byte {
                return Some(u32::from_le_bytes(
                    self.buf[off + 1..off + 5].try_into().unwrap(),
                ));
            } else if b < byte {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        None
    }

    // -- value / pattern-length reads --

    #[inline(always)]
    fn pattern_length(&self, pat_idx: u32) -> u32 {
        let o = self.pat_lengths_off() as usize + pat_idx as usize * 4;
        u32::from_le_bytes(self.buf[o..o + 4].try_into().unwrap())
    }

    fn read_value(&self, pat_idx: u32) -> String {
        let vi = self.values_idx_off() as usize + pat_idx as usize * 8;
        let d_off = u32::from_le_bytes(self.buf[vi..vi + 4].try_into().unwrap()) as usize;
        let d_len = u32::from_le_bytes(self.buf[vi + 4..vi + 8].try_into().unwrap()) as usize;
        let abs = self.values_data_off() as usize + d_off;
        String::from_utf8_lossy(&self.buf[abs..abs + d_len]).into_owned()
    }

    // -- emit matches walking the output-link chain --

    #[inline(always)]
    fn emit(&self, state: u32, end_pos: usize, out: &mut Vec<NerMatch>) {
        let mut s = state;
        loop {
            let vi = self.state_value_idx(s);
            if vi != u32::MAX {
                let plen = self.pattern_length(vi) as usize;
                out.push(NerMatch {
                    start: end_pos - plen,
                    end: end_pos,
                    value: self.read_value(vi),
                });
            }
            let ol = self.state_output_link(s);
            if ol == u32::MAX {
                break;
            }
            s = ol;
        }
    }

    // -- public search --

    /// Scan `text` and return all (possibly overlapping) matches, case-insensitively.
    /// When `delimiters` is `Some`, only matches whose left and right boundaries fall
    /// on a delimiter byte (or the start/end of the text) are returned.
    pub fn find_all_matches(&self, text: &str, delimiters: Option<&[u8]>) -> Vec<NerMatch> {
        if self.buf.len() < HEADER_SIZE || self.num_states() == 0 {
            return Vec::new();
        }

        let mut results = Vec::new();
        let mut state: u32 = 0; // root

        for (i, raw_byte) in text.bytes().enumerate() {
            let b = raw_byte.to_ascii_lowercase();

            loop {
                if let Some(next) = self.goto(state, b) {
                    state = next;
                    break;
                }
                if state == 0 {
                    break;
                }
                state = self.state_failure(state);
            }

            // emit matches ending at this position
            self.emit(state, i + 1, &mut results);
        }

        // Filter by delimiter boundaries if requested
        if let Some(delims) = delimiters {
            let bytes = text.as_bytes();
            results.retain(|m| {
                let left_ok = m.start == 0 || delims.contains(&bytes[m.start - 1]);
                let right_ok = m.end >= bytes.len() || delims.contains(&bytes[m.end]);
                left_ok && right_ok
            });
        }

        results
    }
}
