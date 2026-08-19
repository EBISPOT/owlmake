#!/usr/bin/env python3
"""Compare the named-class subsumption transitive closure of two OWL Functional
Syntax (OFN) files. Used to check owlmake's reasoner against ROBOT axiom-for-axiom
at the inferred-hierarchy level.

Parses SubClassOf / EquivalentClasses over named classes, expands CURIEs, collapses
equivalence cliques into representatives, builds the transitive closure of the
subsumption relation, and diffs the two closures. owl:Thing targets are excluded
(cosmetic). Reports counts of subsumptions present in one but not the other, plus
unsat-class differences.

Usage: closure_diff.py A.ofn B.ofn
Exit 0 iff the closures are identical (0/0).
"""
import re
import sys
from collections import defaultdict


def parse(path):
    """Return (subs, equivs, unsat) where subs is a set of (sub,sup) told
    SubClassOf pairs over named classes, equivs is a list of cliques (sets), and
    unsat is the set of classes asserted equivalent to owl:Nothing."""
    subs = set()
    equivs = []
    unsat = set()
    # A named class reference is either <IRI> or a prefixed name like EFO:0000001.
    cls = r'(?:<[^>]+>|[A-Za-z][\w.-]*:[\w.-]+|owl:Thing|owl:Nothing)'
    sub_re = re.compile(r'SubClassOf\(\s*(' + cls + r')\s+(' + cls + r')\s*\)')
    prefix_re = re.compile(r'Prefix\(\s*([\w.-]*):=<([^>]*)>\s*\)')
    # Prefix map → expand CURIEs to full IRIs so an `<IRI>` (owlmake) and a CURIE
    # `obo:X` (ROBOT) for the same class compare equal.
    prefixes = {}
    lines = open(path, 'r').read().splitlines()
    for line in lines:
        m = prefix_re.search(line)
        if m:
            prefixes[m.group(1)] = m.group(2)

    def norm(tok):
        # owl:Thing / owl:Nothing kept symbolic.
        if tok in ('owl:Thing', 'owl:Nothing'):
            return tok
        if tok.startswith('<') and tok.endswith('>'):
            return tok[1:-1]
        if ':' in tok:
            pfx, local = tok.split(':', 1)
            if pfx in prefixes:
                return prefixes[pfx] + local
        return tok

    def strip_annotations(s):
        # Remove leading axiom-annotation groups `Annotation(...)` (balanced
        # parens, quote-aware) so `SubClassOf(Annotation(...) A B)` parses like
        # `SubClassOf(A B)`. owlmake keeps the asserted axiom's annotations on the
        # subsumption; ROBOT also emits a plain duplicate — without this strip the
        # annotated form is silently dropped, undercounting one side's closure
        # (heavily so on annotation-rich imports like MONDO's mappings).
        res = []
        i, n = 0, len(s)
        while i < n:
            if s.startswith('Annotation(', i):
                depth, in_str, j = 1, False, i + len('Annotation(')
                while j < n and depth > 0:
                    ch = s[j]
                    if in_str:
                        if ch == '"' and s[j - 1] != '\\':
                            in_str = False
                    elif ch == '"':
                        in_str = True
                    elif ch == '(':
                        depth += 1
                    elif ch == ')':
                        depth -= 1
                    j += 1
                i = j
            else:
                res.append(s[i])
                i += 1
        return ''.join(res)

    # EquivalentClasses / SubClassOf may carry axiom annotations; handle the common
    # plain forms. Class expressions (ObjectSomeValuesFrom, etc.) are skipped since
    # we only want named-class atomic subsumptions.
    for line in lines:
        s = strip_annotations(line.strip())
        if s.startswith('SubClassOf('):
            m = sub_re.match(s)
            if m:
                subs.add((norm(m.group(1)), norm(m.group(2))))
        elif s.startswith('EquivalentClasses('):
            names = [norm(n) for n in re.findall(cls, s[len('EquivalentClasses('):])]
            names = [n for n in names if n != 'owl:Thing']
            if 'owl:Nothing' in names:
                for n in names:
                    if n != 'owl:Nothing':
                        unsat.add(n)
            rest = s[len('EquivalentClasses('):]
            if '(' not in rest.replace('))', ')').rstrip(')'):
                if len(names) >= 2:
                    equivs.append(set(names))
    return subs, equivs, unsat


def normalize(s):
    return s


def closure(subs, equivs):
    # union-find over equivalence cliques
    parent = {}

    def find(x):
        parent.setdefault(x, x)
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a, b):
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[ra] = rb

    for clique in equivs:
        cl = list(clique)
        for o in cl[1:]:
            union(cl[0], o)

    # build adjacency on representatives
    adj = defaultdict(set)
    nodes = set()
    for (a, b) in subs:
        ra, rb = find(a), find(b)
        nodes.add(ra)
        nodes.add(rb)
        if ra != rb:
            adj[ra].add(rb)

    # transitive closure via DFS memo
    memo = {}

    def reach(n):
        if n in memo:
            return memo[n]
        memo[n] = set()  # guard cycles
        acc = set()
        for m in adj[n]:
            acc.add(m)
            acc |= reach(m)
        memo[n] = acc
        return acc

    pairs = set()
    for n in nodes:
        for t in reach(n):
            pairs.add((n, t))
    return pairs, find


def main():
    a, b = sys.argv[1], sys.argv[2]
    sa, ea, ua = parse(a)
    sb, eb, ub = parse(b)
    ca, fa = closure(sa, ea)
    cb, fb = closure(sb, eb)

    def strip_thing(pairs):
        return {(x, y) for (x, y) in pairs
                if 'Thing' not in y and 'Thing' not in x}

    ca, cb = strip_thing(ca), strip_thing(cb)
    only_a = ca - cb
    only_b = cb - ca
    print(f"A={a}\n  subs(told)={len(sa)} equivs={len(ea)} unsat={len(ua)} closure={len(ca)}")
    print(f"B={b}\n  subs(told)={len(sb)} equivs={len(eb)} unsat={len(ub)} closure={len(cb)}")
    print(f"closure only in A: {len(only_a)}")
    print(f"closure only in B: {len(only_b)}")
    print(f"unsat only in A: {len(ua - ub)}   unsat only in B: {len(ub - ua)}")
    for label, s in (("ONLY_A", only_a), ("ONLY_B", only_b)):
        for (x, y) in list(sorted(s))[:30]:
            print(f"  {label}: {x} ⊑ {y}")
    ok = not only_a and not only_b and not (ua ^ ub)
    print("RESULT:", "0/0 IDENTICAL" if ok else "DIFFERENCES")
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    main()
