//! Two orderings driven by a caller's comparison, comparison for comparison: the
//! in-order listing of a red-black search tree built by insertion, and a stable
//! run-merging sort. Both make exactly the comparisons, in exactly the order,
//! that their algorithms prescribe. So a comparison that is not a consistent
//! order — `a > b` and `b > a` both claimed — still yields one definite answer,
//! the one the tree's shape and the sort's merge path give it.
//!
//! The elements are indices; `cmp(x, y)` orders element `x` against `y`.

use std::cmp::Ordering;

/// A comparison of two elements by index.
pub trait Cmp: FnMut(usize, usize) -> Ordering {}
impl<F: FnMut(usize, usize) -> Ordering> Cmp for F {}

// ---------------------------------------------------------------------------
// Red-black search tree
// ---------------------------------------------------------------------------

const NIL: usize = usize::MAX;

struct Node {
    key: usize,
    left: usize,
    right: usize,
    parent: usize,
    red: bool,
}

struct Tree {
    nodes: Vec<Node>,
    root: usize,
}

impl Tree {
    fn parent(&self, x: usize) -> usize {
        if x == NIL { NIL } else { self.nodes[x].parent }
    }
    fn left(&self, x: usize) -> usize {
        if x == NIL { NIL } else { self.nodes[x].left }
    }
    fn right(&self, x: usize) -> usize {
        if x == NIL { NIL } else { self.nodes[x].right }
    }
    fn is_red(&self, x: usize) -> bool {
        x != NIL && self.nodes[x].red
    }
    fn set_red(&mut self, x: usize, red: bool) {
        if x != NIL {
            self.nodes[x].red = red;
        }
    }

    fn rotate_left(&mut self, p: usize) {
        if p == NIL {
            return;
        }
        let r = self.nodes[p].right;
        let rl = self.nodes[r].left;
        self.nodes[p].right = rl;
        if rl != NIL {
            self.nodes[rl].parent = p;
        }
        let pp = self.nodes[p].parent;
        self.nodes[r].parent = pp;
        if pp == NIL {
            self.root = r;
        } else if self.nodes[pp].left == p {
            self.nodes[pp].left = r;
        } else {
            self.nodes[pp].right = r;
        }
        self.nodes[r].left = p;
        self.nodes[p].parent = r;
    }

    fn rotate_right(&mut self, p: usize) {
        if p == NIL {
            return;
        }
        let l = self.nodes[p].left;
        let lr = self.nodes[l].right;
        self.nodes[p].left = lr;
        if lr != NIL {
            self.nodes[lr].parent = p;
        }
        let pp = self.nodes[p].parent;
        self.nodes[l].parent = pp;
        if pp == NIL {
            self.root = l;
        } else if self.nodes[pp].right == p {
            self.nodes[pp].right = l;
        } else {
            self.nodes[pp].left = l;
        }
        self.nodes[l].right = p;
        self.nodes[p].parent = l;
    }

    fn fix_after_insertion(&mut self, mut x: usize) {
        self.nodes[x].red = true;
        while x != NIL && x != self.root && self.is_red(self.nodes[x].parent) {
            let p = self.parent(x);
            let g = self.parent(p);
            if p == self.left(g) {
                let y = self.right(g);
                if self.is_red(y) {
                    self.set_red(p, false);
                    self.set_red(y, false);
                    self.set_red(g, true);
                    x = g;
                } else {
                    if x == self.right(p) {
                        x = p;
                        self.rotate_left(x);
                    }
                    let p = self.parent(x);
                    let g = self.parent(p);
                    self.set_red(p, false);
                    self.set_red(g, true);
                    self.rotate_right(g);
                }
            } else {
                let y = self.left(g);
                if self.is_red(y) {
                    self.set_red(p, false);
                    self.set_red(y, false);
                    self.set_red(g, true);
                    x = g;
                } else {
                    if x == self.left(p) {
                        x = p;
                        self.rotate_right(x);
                    }
                    let p = self.parent(x);
                    let g = self.parent(p);
                    self.set_red(p, false);
                    self.set_red(g, true);
                    self.rotate_left(g);
                }
            }
        }
        let root = self.root;
        self.nodes[root].red = false;
    }
}

/// The elements of `insertion`, added one at a time to a red-black search tree
/// ordered by `cmp`, listed in order. An element that compares equal to one
/// already in the tree on its way down is not added.
pub fn tree_set_order(insertion: impl IntoIterator<Item = usize>, mut cmp: impl Cmp) -> Vec<usize> {
    let mut t = Tree { nodes: Vec::new(), root: NIL };
    for key in insertion {
        if t.root == NIL {
            t.nodes.push(Node { key, left: NIL, right: NIL, parent: NIL, red: false });
            t.root = 0;
            continue;
        }
        let mut cur = t.root;
        let mut parent = NIL;
        let mut went_left = false;
        loop {
            parent = cur;
            match cmp(key, t.nodes[cur].key) {
                Ordering::Less => {
                    went_left = true;
                    cur = t.nodes[cur].left;
                }
                Ordering::Greater => {
                    went_left = false;
                    cur = t.nodes[cur].right;
                }
                Ordering::Equal => break,
            }
            if cur == NIL {
                break;
            }
        }
        if cur != NIL {
            continue;
        }
        let e = t.nodes.len();
        t.nodes.push(Node { key, left: NIL, right: NIL, parent, red: false });
        if went_left {
            t.nodes[parent].left = e;
        } else {
            t.nodes[parent].right = e;
        }
        t.fix_after_insertion(e);
    }
    let mut out = Vec::with_capacity(t.nodes.len());
    let mut stack = Vec::new();
    let mut cur = t.root;
    while cur != NIL || !stack.is_empty() {
        while cur != NIL {
            stack.push(cur);
            cur = t.nodes[cur].left;
        }
        let n = stack.pop().expect("non-empty");
        out.push(t.nodes[n].key);
        cur = t.nodes[n].right;
    }
    out
}

// ---------------------------------------------------------------------------
// Run-merging sort
// ---------------------------------------------------------------------------

const MIN_MERGE: usize = 32;
const MIN_GALLOP: isize = 7;

/// The comparison claimed an order no merge can honour: a run was exhausted
/// while the other still held elements it had promised to precede.
#[derive(Debug)]
pub struct InconsistentComparison;

/// Sort `a` by `cmp`: below 32 elements, the leading ascending (or strictly
/// descending, then reversed) run extended by binary insertion; otherwise runs
/// of at least a minimum length, kept on a stack whose lengths stay
/// Fibonacci-like and merged pairwise, switching to galloping when one run
/// keeps winning.
pub fn run_merge_sort(a: &mut [usize], mut cmp: impl Cmp) -> Result<(), InconsistentComparison> {
    let n = a.len();
    if n < 2 {
        return Ok(());
    }
    if n < MIN_MERGE {
        let init = count_run_and_make_ascending(a, 0, n, &mut cmp);
        binary_sort(a, 0, n, init, &mut cmp);
        return Ok(());
    }
    let mut ts = Merger { min_gallop: MIN_GALLOP, runs: Vec::new() };
    let min_run = min_run_length(n);
    let mut lo = 0;
    let mut remaining = n;
    loop {
        let mut run_len = count_run_and_make_ascending(a, lo, n, &mut cmp);
        if run_len < min_run {
            let force = if remaining <= min_run { remaining } else { min_run };
            binary_sort(a, lo, lo + force, lo + run_len, &mut cmp);
            run_len = force;
        }
        ts.runs.push((lo, run_len));
        ts.merge_collapse(a, &mut cmp)?;
        lo += run_len;
        remaining -= run_len;
        if remaining == 0 {
            break;
        }
    }
    ts.merge_force_collapse(a, &mut cmp)
}

fn binary_sort(a: &mut [usize], lo: usize, hi: usize, mut start: usize, cmp: &mut impl Cmp) {
    if start == lo {
        start += 1;
    }
    while start < hi {
        let pivot = a[start];
        let (mut left, mut right) = (lo, start);
        while left < right {
            let mid = (left + right) >> 1;
            if cmp(pivot, a[mid]) == Ordering::Less {
                right = mid;
            } else {
                left = mid + 1;
            }
        }
        a.copy_within(left..start, left + 1);
        a[left] = pivot;
        start += 1;
    }
}

fn count_run_and_make_ascending(a: &mut [usize], lo: usize, hi: usize, cmp: &mut impl Cmp) -> usize {
    let mut run_hi = lo + 1;
    if run_hi == hi {
        return 1;
    }
    let descending = cmp(a[run_hi], a[lo]) == Ordering::Less;
    run_hi += 1;
    if descending {
        while run_hi < hi && cmp(a[run_hi], a[run_hi - 1]) == Ordering::Less {
            run_hi += 1;
        }
        a[lo..run_hi].reverse();
    } else {
        while run_hi < hi && cmp(a[run_hi], a[run_hi - 1]) != Ordering::Less {
            run_hi += 1;
        }
    }
    run_hi - lo
}

fn min_run_length(mut n: usize) -> usize {
    let mut r = 0;
    while n >= MIN_MERGE {
        r |= n & 1;
        n >>= 1;
    }
    n + r
}

/// Where `key` belongs in `arr[base..base+len]`: before the leftmost element it
/// does not exceed, searched outward from `hint`.
fn gallop_left(key: usize, arr: &[usize], base: usize, len: usize, hint: usize, cmp: &mut impl Cmp) -> usize {
    let (len, hint) = (len as isize, hint as isize);
    let at = |i: isize| arr[(base as isize + i) as usize];
    let mut last_ofs: isize = 0;
    let mut ofs: isize = 1;
    if cmp(key, at(hint)) == Ordering::Greater {
        let max_ofs = len - hint;
        while ofs < max_ofs && cmp(key, at(hint + ofs)) == Ordering::Greater {
            last_ofs = ofs;
            ofs = (ofs << 1) + 1;
        }
        if ofs > max_ofs {
            ofs = max_ofs;
        }
        last_ofs += hint;
        ofs += hint;
    } else {
        let max_ofs = hint + 1;
        while ofs < max_ofs && cmp(key, at(hint - ofs)) != Ordering::Greater {
            last_ofs = ofs;
            ofs = (ofs << 1) + 1;
        }
        if ofs > max_ofs {
            ofs = max_ofs;
        }
        let tmp = last_ofs;
        last_ofs = hint - ofs;
        ofs = hint - tmp;
    }
    last_ofs += 1;
    while last_ofs < ofs {
        let m = last_ofs + ((ofs - last_ofs) >> 1);
        if cmp(key, at(m)) == Ordering::Greater {
            last_ofs = m + 1;
        } else {
            ofs = m;
        }
    }
    ofs as usize
}

/// Where `key` belongs in `arr[base..base+len]`: after the rightmost element
/// that does not exceed it, searched outward from `hint`.
fn gallop_right(key: usize, arr: &[usize], base: usize, len: usize, hint: usize, cmp: &mut impl Cmp) -> usize {
    let (len, hint) = (len as isize, hint as isize);
    let at = |i: isize| arr[(base as isize + i) as usize];
    let mut ofs: isize = 1;
    let mut last_ofs: isize = 0;
    if cmp(key, at(hint)) == Ordering::Less {
        let max_ofs = hint + 1;
        while ofs < max_ofs && cmp(key, at(hint - ofs)) == Ordering::Less {
            last_ofs = ofs;
            ofs = (ofs << 1) + 1;
        }
        if ofs > max_ofs {
            ofs = max_ofs;
        }
        let tmp = last_ofs;
        last_ofs = hint - ofs;
        ofs = hint - tmp;
    } else {
        let max_ofs = len - hint;
        while ofs < max_ofs && cmp(key, at(hint + ofs)) != Ordering::Less {
            last_ofs = ofs;
            ofs = (ofs << 1) + 1;
        }
        if ofs > max_ofs {
            ofs = max_ofs;
        }
        last_ofs += hint;
        ofs += hint;
    }
    last_ofs += 1;
    while last_ofs < ofs {
        let m = last_ofs + ((ofs - last_ofs) >> 1);
        if cmp(key, at(m)) == Ordering::Less {
            ofs = m;
        } else {
            last_ofs = m + 1;
        }
    }
    ofs as usize
}

struct Merger {
    min_gallop: isize,
    runs: Vec<(usize, usize)>,
}

impl Merger {
    fn len(&self, i: usize) -> usize {
        self.runs[i].1
    }

    fn merge_collapse(&mut self, a: &mut [usize], cmp: &mut impl Cmp) -> Result<(), InconsistentComparison> {
        while self.runs.len() > 1 {
            let mut n = self.runs.len() - 2;
            if (n > 0 && self.len(n - 1) <= self.len(n) + self.len(n + 1))
                || (n > 1 && self.len(n - 2) <= self.len(n) + self.len(n - 1))
            {
                if self.len(n - 1) < self.len(n + 1) {
                    n -= 1;
                }
            } else if self.len(n) > self.len(n + 1) {
                break;
            }
            self.merge_at(n, a, cmp)?;
        }
        Ok(())
    }

    fn merge_force_collapse(&mut self, a: &mut [usize], cmp: &mut impl Cmp) -> Result<(), InconsistentComparison> {
        while self.runs.len() > 1 {
            let mut n = self.runs.len() - 2;
            if n > 0 && self.len(n - 1) < self.len(n + 1) {
                n -= 1;
            }
            self.merge_at(n, a, cmp)?;
        }
        Ok(())
    }

    fn merge_at(&mut self, i: usize, a: &mut [usize], cmp: &mut impl Cmp) -> Result<(), InconsistentComparison> {
        let (base1, len1) = self.runs[i];
        let (base2, len2) = self.runs[i + 1];
        self.runs[i].1 = len1 + len2;
        self.runs.remove(i + 1);
        let k = gallop_right(a[base2], a, base1, len1, 0, cmp);
        let base1 = base1 + k;
        let len1 = len1 - k;
        if len1 == 0 {
            return Ok(());
        }
        let len2 = gallop_left(a[base1 + len1 - 1], a, base2, len2, len2 - 1, cmp);
        if len2 == 0 {
            return Ok(());
        }
        if len1 <= len2 {
            self.merge_lo(a, base1, len1, base2, len2, cmp)
        } else {
            self.merge_hi(a, base1, len1, base2, len2, cmp)
        }
    }

    fn merge_lo(
        &mut self,
        a: &mut [usize],
        base1: usize,
        mut len1: usize,
        base2: usize,
        mut len2: usize,
        cmp: &mut impl Cmp,
    ) -> Result<(), InconsistentComparison> {
        let tmp: Vec<usize> = a[base1..base1 + len1].to_vec();
        let mut c1 = 0usize;
        let mut c2 = base2;
        let mut dest = base1;
        a[dest] = a[c2];
        dest += 1;
        c2 += 1;
        len2 -= 1;
        if len2 == 0 {
            a[dest..dest + len1].copy_from_slice(&tmp[c1..c1 + len1]);
            return Ok(());
        }
        if len1 == 1 {
            a.copy_within(c2..c2 + len2, dest);
            a[dest + len2] = tmp[c1];
            return Ok(());
        }
        let mut min_gallop = self.min_gallop;
        'outer: loop {
            let mut count1: isize = 0;
            let mut count2: isize = 0;
            loop {
                if cmp(a[c2], tmp[c1]) == Ordering::Less {
                    a[dest] = a[c2];
                    dest += 1;
                    c2 += 1;
                    count2 += 1;
                    count1 = 0;
                    len2 -= 1;
                    if len2 == 0 {
                        break 'outer;
                    }
                } else {
                    a[dest] = tmp[c1];
                    dest += 1;
                    c1 += 1;
                    count1 += 1;
                    count2 = 0;
                    len1 -= 1;
                    if len1 == 1 {
                        break 'outer;
                    }
                }
                if (count1 | count2) >= min_gallop {
                    break;
                }
            }
            loop {
                let g = gallop_right(a[c2], &tmp, c1, len1, 0, cmp);
                count1 = g as isize;
                if g != 0 {
                    a[dest..dest + g].copy_from_slice(&tmp[c1..c1 + g]);
                    dest += g;
                    c1 += g;
                    len1 -= g;
                    if len1 <= 1 {
                        break 'outer;
                    }
                }
                a[dest] = a[c2];
                dest += 1;
                c2 += 1;
                len2 -= 1;
                if len2 == 0 {
                    break 'outer;
                }
                let g = gallop_left(tmp[c1], a, c2, len2, 0, cmp);
                count2 = g as isize;
                if g != 0 {
                    a.copy_within(c2..c2 + g, dest);
                    dest += g;
                    c2 += g;
                    len2 -= g;
                    if len2 == 0 {
                        break 'outer;
                    }
                }
                a[dest] = tmp[c1];
                dest += 1;
                c1 += 1;
                len1 -= 1;
                if len1 == 1 {
                    break 'outer;
                }
                min_gallop -= 1;
                if !(count1 >= MIN_GALLOP || count2 >= MIN_GALLOP) {
                    break;
                }
            }
            if min_gallop < 0 {
                min_gallop = 0;
            }
            min_gallop += 2;
        }
        self.min_gallop = min_gallop.max(1);
        if len1 == 1 {
            a.copy_within(c2..c2 + len2, dest);
            a[dest + len2] = tmp[c1];
        } else if len1 == 0 {
            return Err(InconsistentComparison);
        } else {
            a[dest..dest + len1].copy_from_slice(&tmp[c1..c1 + len1]);
        }
        Ok(())
    }

    fn merge_hi(
        &mut self,
        a: &mut [usize],
        base1: usize,
        mut len1: usize,
        base2: usize,
        mut len2: usize,
        cmp: &mut impl Cmp,
    ) -> Result<(), InconsistentComparison> {
        let tmp: Vec<usize> = a[base2..base2 + len2].to_vec();
        let mut c1: isize = (base1 + len1) as isize - 1;
        let mut c2: isize = len2 as isize - 1;
        let mut dest: isize = (base2 + len2) as isize - 1;
        let at = |i: isize| i as usize;
        a[at(dest)] = a[at(c1)];
        dest -= 1;
        c1 -= 1;
        len1 -= 1;
        if len1 == 0 {
            let start = at(dest - (len2 as isize - 1));
            a[start..start + len2].copy_from_slice(&tmp[..len2]);
            return Ok(());
        }
        if len2 == 1 {
            dest -= len1 as isize;
            c1 -= len1 as isize;
            a.copy_within(at(c1 + 1)..at(c1 + 1) + len1, at(dest + 1));
            a[at(dest)] = tmp[at(c2)];
            return Ok(());
        }
        let mut min_gallop = self.min_gallop;
        'outer: loop {
            let mut count1: isize = 0;
            let mut count2: isize = 0;
            loop {
                if cmp(tmp[at(c2)], a[at(c1)]) == Ordering::Less {
                    a[at(dest)] = a[at(c1)];
                    dest -= 1;
                    c1 -= 1;
                    count1 += 1;
                    count2 = 0;
                    len1 -= 1;
                    if len1 == 0 {
                        break 'outer;
                    }
                } else {
                    a[at(dest)] = tmp[at(c2)];
                    dest -= 1;
                    c2 -= 1;
                    count2 += 1;
                    count1 = 0;
                    len2 -= 1;
                    if len2 == 1 {
                        break 'outer;
                    }
                }
                if (count1 | count2) >= min_gallop {
                    break;
                }
            }
            loop {
                let g = len1 - gallop_right(tmp[at(c2)], a, base1, len1, len1 - 1, cmp);
                count1 = g as isize;
                if g != 0 {
                    dest -= g as isize;
                    c1 -= g as isize;
                    len1 -= g;
                    a.copy_within(at(c1 + 1)..at(c1 + 1) + g, at(dest + 1));
                    if len1 == 0 {
                        break 'outer;
                    }
                }
                a[at(dest)] = tmp[at(c2)];
                dest -= 1;
                c2 -= 1;
                len2 -= 1;
                if len2 == 1 {
                    break 'outer;
                }
                let g = len2 - gallop_left(a[at(c1)], &tmp, 0, len2, len2 - 1, cmp);
                count2 = g as isize;
                if g != 0 {
                    dest -= g as isize;
                    c2 -= g as isize;
                    len2 -= g;
                    a[at(dest + 1)..at(dest + 1) + g].copy_from_slice(&tmp[at(c2 + 1)..at(c2 + 1) + g]);
                    if len2 <= 1 {
                        break 'outer;
                    }
                }
                a[at(dest)] = a[at(c1)];
                dest -= 1;
                c1 -= 1;
                len1 -= 1;
                if len1 == 0 {
                    break 'outer;
                }
                min_gallop -= 1;
                if !(count1 >= MIN_GALLOP || count2 >= MIN_GALLOP) {
                    break;
                }
            }
            if min_gallop < 0 {
                min_gallop = 0;
            }
            min_gallop += 2;
        }
        self.min_gallop = min_gallop.max(1);
        if len2 == 1 {
            dest -= len1 as isize;
            c1 -= len1 as isize;
            a.copy_within(at(c1 + 1)..at(c1 + 1) + len1, at(dest + 1));
            a[at(dest)] = tmp[at(c2)];
        } else if len2 == 0 {
            return Err(InconsistentComparison);
        } else {
            let start = at(dest - (len2 as isize - 1));
            a[start..start + len2].copy_from_slice(&tmp[..len2]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// SplitMix64, the stream the expected results were generated from.
    struct Mix(u64);
    impl Mix {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn shuffle(&mut self, a: &mut [usize]) {
            for i in (1..a.len()).rev() {
                let j = self.below(i + 1);
                a.swap(i, j);
            }
        }
    }

    fn fnv(v: &[usize]) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &x in v {
            h ^= x as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:x}")
    }

    /// Each case — keys, a key order broken on generated pairs (each pair
    /// compares greater both ways), an insertion order and a shuffled list — is
    /// generated from a SplitMix64 stream. `tests/fixtures/tree-sort/hashes.txt`
    /// holds, per case, the FNV-1a hashes of what Java 11 gave for the same case:
    /// the `java.util.TreeSet` listing, `ArrayList.sort(null)` of that listing,
    /// and `ArrayList.sort(null)` of the shuffled list, whose runs merge and
    /// gallop. Sizes run to 1,000.
    #[test]
    fn tree_and_sort_follow_the_jdk_under_a_broken_comparison() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tree-sort/hashes.txt");
        let text = std::fs::read_to_string(path).unwrap();
        let mut long = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split(' ').collect();
            let c: u64 = f[0].parse().unwrap();
            let mut m = Mix(0x5EED_0000 + c);
            let n = match c % 4 {
                0 => 2 + m.below(30),
                1 => 32 + m.below(200),
                2 => 200 + m.below(800),
                _ => 2 + m.below(300),
            };
            let range = if c % 3 == 0 { n * 4 } else { (n / 3).max(1) };
            let keys: Vec<usize> = (0..n).map(|_| m.below(range)).collect();
            let nb = if c % 3 == 2 { m.below(1 + n / 2) } else { m.below(1 + n / 8) };
            let mut broken: HashSet<(usize, usize)> = HashSet::new();
            for _ in 0..nb {
                let (x, y) = (m.below(n), m.below(n));
                if x != y {
                    broken.insert((x, y));
                    broken.insert((y, x));
                }
            }
            let cmp = |x: usize, y: usize| {
                if x == y {
                    Ordering::Equal
                } else if broken.contains(&(x, y)) {
                    Ordering::Greater
                } else {
                    keys[x].cmp(&keys[y])
                }
            };
            let mut ins: Vec<usize> = (0..n).collect();
            m.shuffle(&mut ins);
            if c % 2 == 0 {
                ins.sort_by_key(|&p| keys[p] / 5);
            }
            let mut listing = tree_set_order(ins, cmp);
            assert_eq!(fnv(&listing), f[1], "case {c}: tree listing");
            match run_merge_sort(&mut listing, cmp) {
                Ok(()) => assert_eq!(fnv(&listing), f[2], "case {c}: sorted listing"),
                Err(_) => assert_eq!("throws", f[2], "case {c}: sorted listing"),
            }
            let mut shuffled: Vec<usize> = (0..n).collect();
            m.shuffle(&mut shuffled);
            if c % 5 == 0 {
                shuffled.sort_by_key(|&p| keys[p] / 3);
            }
            match run_merge_sort(&mut shuffled, cmp) {
                Ok(()) => assert_eq!(fnv(&shuffled), f[3], "case {c}: sorted shuffle"),
                Err(_) => assert_eq!("throws", f[3], "case {c}: sorted shuffle"),
            }
            if n >= MIN_MERGE {
                long += 1;
            }
        }
        assert!(long > 0);
    }
}
