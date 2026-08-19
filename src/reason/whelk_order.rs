//! The order the EL reasoner visits a concept's subsumers in.
//!
//! Which member of an equivalence clique becomes a class's direct superclass is
//! decided by that order: the reduction folds over the subsumer set and the
//! first clique member it meets stands for the clique, every later one hidden
//! behind it. `PR:000001338 ⊑ PR:000000001` is asserted, `PR:000000001` and
//! `CHEBI:36080` are equivalent, and the direct superclass is `CHEBI:36080` —
//! the one the fold reaches first.
//!
//! The set is a hash trie, so its order is `crate::hash_trie`'s (see there). The
//! hash it keys on is the concept's structural one — a Murmur3 product hash over
//! the concept kind's name and its fields — so it is a function of the concept
//! alone, and the trie shape a function of the whole subsumer set.

use std::collections::HashMap;

use whelk::whelk::model::{ConceptData, ConceptId, Interner};

/// `MurmurHash3.productSeed`.
const PRODUCT_SEED: i32 = 0xcafebabeu32 as i32;

fn rotl(x: i32, r: u32) -> i32 {
    ((x as u32).rotate_left(r)) as i32
}

fn mix_last(hash: i32, data: i32) -> i32 {
    let mut k = data;
    k = k.wrapping_mul(0xcc9e2d51u32 as i32);
    k = rotl(k, 15);
    k = k.wrapping_mul(0x1b873593);
    hash ^ k
}

fn mix(hash: i32, data: i32) -> i32 {
    let h = rotl(mix_last(hash, data), 13);
    h.wrapping_mul(5).wrapping_add(0xe6546b64u32 as i32)
}

fn avalanche(hash: i32) -> i32 {
    let mut h = hash as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85ebca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^= h >> 16;
    h as i32
}

fn finalize_hash(hash: i32, length: i32) -> i32 {
    avalanche(hash ^ length)
}

/// The case-class hash: the seed, the kind's name, then each field.
fn product_hash(prefix: &str, fields: &[i32]) -> i32 {
    let mut h = PRODUCT_SEED;
    h = mix(h, crate::owlapi_hash::java_string_hash(prefix));
    for f in fields {
        h = mix(h, *f);
    }
    finalize_hash(h, fields.len() as i32)
}

/// An unordered collection's hash — a set's, which does not depend on iteration
/// order.
fn unordered_hash(elems: &[i32], seed: i32) -> i32 {
    let (mut a, mut b, mut n) = (0i32, 0i32, 0i32);
    let mut c: i32 = 1;
    for &h in elems {
        a = a.wrapping_add(h);
        b ^= h;
        c = c.wrapping_mul(h | 1);
        n += 1;
    }
    let mut h = seed;
    h = mix(h, a);
    h = mix(h, b);
    h = mix_last(h, c);
    finalize_hash(h, n)
}

/// The hash of one concept, memoized over the shared sub-expressions.
fn concept_hash(interner: &Interner, id: ConceptId, memo: &mut HashMap<ConceptId, i32>) -> i32 {
    if let Some(h) = memo.get(&id) {
        return *h;
    }
    let h = match interner.concept_data(id) {
        ConceptData::AtomicConcept(name) => {
            product_hash("AtomicConcept", &[crate::owlapi_hash::java_string_hash(name)])
        }
        ConceptData::Conjunction { left, right } => {
            let (l, r) = (*left, *right);
            let lh = concept_hash(interner, l, memo);
            let rh = concept_hash(interner, r, memo);
            product_hash("Conjunction", &[lh, rh])
        }
        ConceptData::ExistentialRestriction { role, concept } => {
            let (ro, co) = (*role, *concept);
            let rh = product_hash("Role", &[crate::owlapi_hash::java_string_hash(interner.role_name(ro))]);
            let ch = concept_hash(interner, co, memo);
            product_hash("ExistentialRestriction", &[rh, ch])
        }
        ConceptData::RoleTarget { range, concept } => {
            let (ra, co) = (*range, *concept);
            let rh = concept_hash(interner, ra, memo);
            let ch = concept_hash(interner, co, memo);
            product_hash("RoleTarget", &[rh, ch])
        }
        ConceptData::Disjunction(operands) => {
            let ops: Vec<ConceptId> = operands.iter().copied().collect();
            let hs: Vec<i32> = ops.into_iter().map(|o| concept_hash(interner, o, memo)).collect();
            product_hash("Disjunction", &[unordered_hash(&hs, crate::owlapi_hash::java_string_hash("Set"))])
        }
        ConceptData::Complement(c) => {
            let c = *c;
            let ch = concept_hash(interner, c, memo);
            product_hash("Complement", &[ch])
        }
        ConceptData::Nominal(i) => {
            let ih = product_hash("Individual", &[crate::owlapi_hash::java_string_hash(interner.individual_name(*i))]);
            product_hash("Nominal", &[ih])
        }
        ConceptData::SelfRestriction(role) => {
            let rh = product_hash("Role", &[crate::owlapi_hash::java_string_hash(interner.role_name(*role))]);
            product_hash("SelfRestriction", &[rh])
        }
    };
    memo.insert(id, h);
    h
}

/// The order the reasoner visits `subsumers` in.
pub fn visit_order(
    interner: &Interner,
    subsumers: &[ConceptId],
    memo: &mut HashMap<ConceptId, i32>,
) -> Vec<ConceptId> {
    let items: Vec<(ConceptId, i32)> = subsumers
        .iter()
        .map(|&id| (id, concept_hash(interner, id, memo)))
        .collect();
    crate::hash_trie::order(&items)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hashes are the ones the reasoner's own model computes, checked
    /// against `AtomicConcept(iri).hashCode` for CL's protein clique and the
    /// classes around it.
    #[test]
    fn atomic_concept_hashes_are_the_models() {
        let h = |iri: &str| product_hash("AtomicConcept", &[crate::owlapi_hash::java_string_hash(iri)]);
        assert_eq!(h("http://purl.obolibrary.org/obo/CHEBI_36080"), -924874776);
        assert_eq!(h("http://purl.obolibrary.org/obo/PR_000000001"), -23182629);
        assert_eq!(h("http://purl.obolibrary.org/obo/PR_000001338"), -179377773);
        assert_eq!(h("http://purl.obolibrary.org/obo/BFO_0000002"), 474258681);
        assert_eq!(h("http://purl.obolibrary.org/obo/COB_0000013"), 620773403);
        assert_eq!(h("http://purl.obolibrary.org/obo/PR_000018263"), 1806750849);
        assert_eq!(h("http://www.w3.org/2002/07/owl#Thing"), 1683162663);
    }

    /// …and the trie walk reproduces the set's own iteration order:
    /// `PR:000001338`'s subsumers come out ⊤, CHEBI:36080, COB:0000013,
    /// PR:000018263, BFO:0000002, PR:000000001, PR:000001338 — CHEBI ahead of
    /// PR:000000001, which is why it is the direct superclass.
    #[test]
    fn the_walk_reproduces_the_sets_order() {
        let iris = [
            "http://purl.obolibrary.org/obo/PR_000001338",
            "http://purl.obolibrary.org/obo/BFO_0000002",
            "http://purl.obolibrary.org/obo/CHEBI_36080",
            "http://purl.obolibrary.org/obo/COB_0000013",
            "http://purl.obolibrary.org/obo/PR_000000001",
            "http://purl.obolibrary.org/obo/PR_000018263",
            "http://www.w3.org/2002/07/owl#Thing",
        ];
        let items: Vec<(usize, i32)> = iris
            .iter()
            .enumerate()
            .map(|(i, iri)| {
                (i, product_hash("AtomicConcept", &[crate::owlapi_hash::java_string_hash(iri)]))
            })
            .collect();
        let out = crate::hash_trie::order(&items);
        let order: Vec<&str> = out.iter().map(|&i| iris[i]).collect();
        assert_eq!(
            order,
            vec![
                "http://www.w3.org/2002/07/owl#Thing",
                "http://purl.obolibrary.org/obo/CHEBI_36080",
                "http://purl.obolibrary.org/obo/COB_0000013",
                "http://purl.obolibrary.org/obo/PR_000018263",
                "http://purl.obolibrary.org/obo/BFO_0000002",
                "http://purl.obolibrary.org/obo/PR_000000001",
                "http://purl.obolibrary.org/obo/PR_000001338",
            ]
        );
    }
}
