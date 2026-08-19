//! The iteration order of a persistent hash-trie set.
//!
//! A set built as a hash trie has no insertion order to remember: what it yields
//! is decided by its elements' hashes alone. At each level the entries stored
//! inline come first, in ascending slot order, then the sub-nodes in ascending
//! slot order, recursively; a slot is five bits of an element's scrambled hash,
//! taken from the least significant end.
//!
//! Two things are ordered this way and must reproduce it exactly: the subsumer
//! set the EL reasoner folds over, which decides which member of an equivalence
//! clique becomes a class's direct superclass, and the term list a DOSDP pattern
//! writes, which is a set of IRI strings.

/// The trie's scrambling of a hash: only the low bits pick a slot, so the high
/// ones are folded down into them.
pub fn improve(hcode: i32) -> u32 {
    let mut h = hcode;
    h = h.wrapping_add(!(h << 9));
    h ^= ((h as u32) >> 14) as i32;
    h = h.wrapping_add(h << 4);
    h ^= ((h as u32) >> 10) as i32;
    h as u32
}

/// The order a set of `(element, hash)` pairs is iterated in. The hashes are the
/// elements' own — this scrambles them.
pub fn order<T: Clone + Ord>(items: &[(T, i32)]) -> Vec<T> {
    let scrambled: Vec<(T, u32)> =
        items.iter().map(|(v, h)| (v.clone(), improve(*h))).collect();
    let mut out = Vec::with_capacity(items.len());
    walk(&scrambled, 0, &mut out);
    out
}

fn walk<T: Clone + Ord>(group: &[(T, u32)], shift: u32, out: &mut Vec<T>) {
    if group.len() == 1 {
        out.push(group[0].0.clone());
        return;
    }
    // Past the 32 bits a hash has, equal-hash elements share a collision node and
    // keep the order they were added in; the sets are small enough that the
    // elements' own order stands in for it.
    if shift >= 32 {
        let mut rest: Vec<T> = group.iter().map(|(v, _)| v.clone()).collect();
        rest.sort();
        out.extend(rest);
        return;
    }
    let mut slots: Vec<Vec<(T, u32)>> = vec![Vec::new(); 32];
    for it in group {
        slots[((it.1 >> shift) & 31) as usize].push(it.clone());
    }
    for slot in slots.iter() {
        if slot.len() == 1 {
            out.push(slot[0].0.clone());
        }
    }
    for slot in slots.iter() {
        if slot.len() > 1 {
            walk(slot, shift + 5, out);
        }
    }
}
