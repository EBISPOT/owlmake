#!/usr/bin/env python3
"""Compare two ontology SQL databases table by table.

The byte comparison a release artefact is normally held to does not apply to
these: a `.db` records the order its rows were inserted in, and the reference
tool computes the relation graph in parallel and writes each row as it lands, so
two of its own runs of the same input differ by hundreds of thousands of bytes.
What is comparable is the content — the schema, and every row of every table —
which is what this reports, separating a difference in row ORDER from a
difference in the rows themselves.

Usage: semsql_compare.py <a.db> <b.db>
"""
import sqlite3
import sys


def names(conn, kind):
    q = "select name from sqlite_master where type=? order by name"
    return [r[0] for r in conn.execute(q, (kind,))]


def schema(conn, kind):
    """Each object's name AND its definition — a view that selects something
    else is a different view under the same name."""
    q = "select name, sql from sqlite_master where type=? order by name"
    return [tuple(r) for r in conn.execute(q, (kind,))]


def rows(conn, table):
    return [tuple(r) for r in conn.execute(f"select * from {table}")]


def main(a_path, b_path):
    a, b = sqlite3.connect(a_path), sqlite3.connect(b_path)
    ok = True
    for kind in ("table", "view", "index"):
        na, nb = schema(a, kind), schema(b, kind)
        label = {"table": "tables", "view": "views", "index": "indexes"}[kind]
        if na != nb:
            ok = False
            only_a = [n for n in na if n not in nb]
            only_b = [n for n in nb if n not in na]
            print(f"{label} DIFFER: only-a={[n for n, _ in only_a]} only-b={[n for n, _ in only_b]}")
        else:
            print(f"{len(na):4d} {label} identical")
    for t in names(a, "table"):
        ra, rb = rows(a, t), rows(b, t)
        sa, sb = sorted(map(repr, ra)), sorted(map(repr, rb))
        if sa == sb:
            note = "identical" if ra == rb else "same rows, different insertion order"
            print(f"{t:24s} {len(ra):8d} rows  {note}")
            continue
        ok = False
        sa_set, sb_set = set(sa), set(sb)
        print(f"{t:24s} {len(ra):8d} vs {len(rb):8d} rows  DIFFER "
              f"(only-a {len(sa_set - sb_set)}, only-b {len(sb_set - sa_set)})")
        for r in sorted(sa_set - sb_set)[:5]:
            print(f"    only in {a_path}: {r}")
        for r in sorted(sb_set - sa_set)[:5]:
            print(f"    only in {b_path}: {r}")
    return 0 if ok else 1


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print(__doc__)
        sys.exit(2)
    sys.exit(main(sys.argv[1], sys.argv[2]))
