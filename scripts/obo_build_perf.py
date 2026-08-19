#!/usr/bin/env python3
"""End-to-end build benchmark: ODK `make` (ROBOT) vs `owlmake make`.

For each ODK ontology repo it cleans the release products, builds the main
product BOTH ways — the stock ODK Makefile driven by real ROBOT, and
`owlmake make` — timing each end-to-end build, then `robot diff`s the two
artefacts to confirm they are axiom-identical, and plots the comparison.

This measures the WHOLE release pipeline (merge -> reason -> relax -> reduce ->
annotate -> serialize), not just reasoning.

Usage:
    scripts/obo_build_perf.py /path/to/bspo            # a checked-out ODK repo
    scripts/obo_build_perf.py repos/*/                 # several repos
    scripts/obo_build_perf.py --product cl.owl /path/to/cl
    scripts/obo_build_perf.py --imp --runs 2 /path/to/bspo   # also refresh imports

Paths (auto-discovered, or via env / flags):
    OWLMAKE_BIN   owlmake binary  (default: target/release/owlmake)
    ROBOT_JAR     robot.jar       (default: ./robot.jar)
Notes:
  * `make` and a `robot` on PATH are required for the ODK side; if `robot` is not
    on PATH a wrapper around ROBOT_JAR is synthesized for the build.
  * By default the ODK build runs with IMP=false (imports are not refreshed) so
    the comparison is the release build over the already-built imports, the same
    inputs owlmake uses. Pass --imp to include ODK's import refresh.
"""
import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def find_default(paths):
    for p in paths:
        if p and Path(p).exists():
            return str(p)
    return None


def discover_repo(arg):
    """Return (src_ontology_dir, ontology_id) for an ODK repo path."""
    root = Path(arg).resolve()
    # accept the repo root, the src/ontology dir, or an -odk.yaml file
    if root.is_file() and root.name.endswith("-odk.yaml"):
        srcdir = root.parent
    elif (root / "src" / "ontology").is_dir():
        srcdir = root / "src" / "ontology"
    elif root.name == "ontology" and (root / "Makefile").exists():
        srcdir = root
    elif (root / "Makefile").exists():
        srcdir = root
    else:
        return None
    # ontology id from a *-odk.yaml, else the Makefile's ONT=, else dir name
    oid = None
    for y in srcdir.glob("*-odk.yaml"):
        oid = y.name[:-len("-odk.yaml")]
        break
    if not oid:
        mk = srcdir / "Makefile"
        if mk.exists():
            m = re.search(r"^ONT\s*=\s*(\S+)", mk.read_text(), re.M)
            if m:
                oid = m.group(1)
    if not oid:
        oid = root.name if root.name != "ontology" else root.parent.parent.name
    return srcdir, oid


def products(srcdir, oid):
    pats = [f"{oid}.owl", f"{oid}-full.owl", f"{oid}-base.owl", f"{oid}-simple.owl",
            f"{oid}-basic.owl", f"{oid}.obo", f"{oid}.json"]
    return [srcdir / p for p in pats]


def clean(srcdir, oid):
    for p in products(srcdir, oid):
        try:
            p.unlink()
        except FileNotFoundError:
            pass
    shutil.rmtree(srcdir / "tmp", ignore_errors=True)


def timed_build(cmd, cwd, env, timeout):
    t0 = time.perf_counter()
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, stdout=subprocess.DEVNULL,
                           stderr=subprocess.PIPE, timeout=timeout)
        return time.perf_counter() - t0, p.returncode, p.stderr.decode("utf-8", "replace")[-500:]
    except subprocess.TimeoutExpired:
        return float("inf"), 124, "timeout"


def robot_diff(robot, left, right):
    p = subprocess.run(["java", "-jar", robot, "diff", "--left", str(left), "--right", str(right)],
                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    out = p.stdout.decode("utf-8", "replace")
    if "Ontologies are identical" in out:
        return (0, 0, out)
    m1 = re.search(r"(\d+) axioms in left ontology but not in right", out)
    m2 = re.search(r"(\d+) axioms in right ontology but not in left", out)
    return (int(m1.group(1)) if m1 else -1, int(m2.group(1)) if m2 else -1, out)


def bench_repo(arg, owlmake, robot, robot_path_dir, product, imp, runs, timeout):
    disc = discover_repo(arg)
    if not disc:
        print(f"  not an ODK repo (no Makefile/src/ontology): {arg}")
        return None
    srcdir, oid = disc
    prod = product or f"{oid}.owl"
    print(f"  repo={srcdir}  id={oid}  product={prod}")

    env = dict(os.environ)
    env["PATH"] = robot_path_dir + os.pathsep + env.get("PATH", "")

    # ODK / ROBOT build (stock Makefile). IMP=false by default for a like-for-like
    # release build over already-built imports.
    odk_cmd = ["make", prod] + ([] if imp else ["IMP=false", "MIR=false", "PAT=false"])
    t_odk, rc_odk, err_odk = float("inf"), 1, ""
    for _ in range(runs):
        clean(srcdir, oid)
        t, rc, err = timed_build(odk_cmd, srcdir, env, timeout)
        t_odk, rc_odk, err_odk = (min(t_odk, t), rc, err) if rc == 0 else (t_odk, rc, err)
    odk_art = (srcdir / prod)
    odk_saved = None
    if rc_odk == 0 and odk_art.exists():
        odk_saved = Path(tempfile.gettempdir()) / f"odk_{oid}_{prod}"
        shutil.copy(odk_art, odk_saved)

    # owlmake build
    owl_cmd = [owlmake, "make", prod]
    t_owl, rc_owl, err_owl = float("inf"), 1, ""
    for _ in range(runs):
        clean(srcdir, oid)
        t, rc, err = timed_build(owl_cmd, srcdir, env, timeout)
        t_owl, rc_owl, err_owl = (min(t_owl, t), rc, err) if rc == 0 else (t_owl, rc, err)
    owl_art = (srcdir / prod)
    owl_saved = None
    if rc_owl == 0 and owl_art.exists():
        owl_saved = Path(tempfile.gettempdir()) / f"owlmake_{oid}_{prod}"
        shutil.copy(owl_art, owl_saved)

    diff = None
    if odk_saved and owl_saved:
        diff = robot_diff(robot, owl_saved, odk_saved)
    return dict(id=oid, product=prod, odk=t_odk, owlmake=t_owl, rc_odk=rc_odk,
                rc_owl=rc_owl, err_odk=err_odk, err_owl=err_owl, diff=diff)


def status_tag(d):
    if d["diff"] is None:
        return f"build failed (odk={d['rc_odk']},owl={d['rc_owl']})"
    a, b, _ = d["diff"]
    if a == 0 and b == 0:
        return "IDENTICAL"
    return f"DIFF onlyODK={a} onlyOwlmake={b}"


def plot(results, out_png, imp):
    ok = [r for r in results if r and r["odk"] != float("inf") and r["owlmake"] != float("inf")]
    if not ok:
        print("no successful builds to plot")
        return
    ok.sort(key=lambda r: r["owlmake"])
    label = []
    for r in ok:
        spd = r["odk"] / r["owlmake"] if r["owlmake"] else float("nan")
        tag = "OK" if (r["diff"] and r["diff"][0] == 0 and r["diff"][1] == 0) else "x"
        label.append(f"{r['id']}  ({spd:.1f}x, {tag})")
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        _plot_svg(ok, label, out_png, imp)
        return
    y = range(len(ok))
    h = 0.38
    fig, ax = plt.subplots(figsize=(9, max(3, 0.6 * len(ok) + 1.6)))
    ax.barh([i + h / 2 for i in y], [r["owlmake"] for r in ok], height=h, label="owlmake make", color="#2c7fb8")
    ax.barh([i - h / 2 for i in y], [r["odk"] for r in ok], height=h, label="ODK make (ROBOT)", color="#d95f0e")
    ax.set_yticks(list(y)); ax.set_yticklabels(label)
    ax.set_xlabel("end-to-end build wall-clock (s; lower is better)")
    ax.set_title("ODK `make` vs `owlmake make` — full release build\n"
                 f"OK = artefacts axiom-identical (robot diff); N× = speedup{'  [imports refreshed]' if imp else ''}")
    ax.legend(loc="lower right"); ax.grid(axis="x", alpha=0.3)
    fig.tight_layout(); fig.savefig(out_png, dpi=130)
    print(f"\nplot written to {out_png}")


def _plot_svg(ok, label, out_png, imp):
    out = Path(out_png).with_suffix(".svg")
    W, rowh, pad, left = 900, 46, 60, 300
    H = pad * 2 + rowh * len(ok)
    vmax = max(max(r["odk"], r["owlmake"]) for r in ok) or 1.0
    span = W - left - 90
    s = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="sans-serif">',
         f'<rect width="{W}" height="{H}" fill="white"/>',
         f'<text x="{W/2}" y="26" text-anchor="middle" font-size="15">ODK make vs owlmake make — e2e build (s, lower is better)</text>']
    for i, r in enumerate(ok):
        yy = pad + i * rowh
        s.append(f'<text x="10" y="{yy+rowh/2+4:.0f}" font-size="13">{label[i]}</text>')
        s.append(f'<rect x="{left}" y="{yy+4:.0f}" width="{max(span*r["owlmake"]/vmax,0.5):.1f}" height="{rowh/2-5:.0f}" fill="#2c7fb8"/>')
        s.append(f'<rect x="{left}" y="{yy+rowh/2+1:.0f}" width="{max(span*r["odk"]/vmax,0.5):.1f}" height="{rowh/2-5:.0f}" fill="#d95f0e"/>')
        s.append(f'<text x="{left+span*r["owlmake"]/vmax+5:.0f}" y="{yy+rowh/2:.0f}" font-size="11">{r["owlmake"]:.2f}s owlmake</text>')
        s.append(f'<text x="{left+span*r["odk"]/vmax+5:.0f}" y="{yy+rowh-4:.0f}" font-size="11">{r["odk"]:.2f}s ODK</text>')
    s.append("</svg>")
    out.write_text("\n".join(s))
    print(f"\nplot written to {out} (matplotlib unavailable; wrote SVG)")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("repos", nargs="+", help="ODK repo dirs (root, src/ontology, or -odk.yaml)")
    ap.add_argument("--product", default=None, help="product to build (default: <id>.owl)")
    ap.add_argument("--imp", action="store_true", help="let ODK refresh imports too (default: IMP=false)")
    ap.add_argument("--runs", type=int, default=1, help="builds per tool, best wall-clock taken")
    ap.add_argument("--timeout", type=int, default=3600)
    ap.add_argument("--owlmake", default=os.environ.get("OWLMAKE_BIN"))
    ap.add_argument("--robot", default=os.environ.get("ROBOT_JAR"))
    ap.add_argument("--out", default="/tmp/obo_build_perf.png")
    a = ap.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    owlmake = find_default([a.owlmake, repo_root / "target/release/owlmake", repo_root / "target/profiling/owlmake"])
    robot = find_default([a.robot, repo_root / "robot.jar", shutil.which("robot")])
    if not owlmake:
        sys.exit("owlmake not found (build it or pass --owlmake)")
    if not robot:
        sys.exit("robot.jar not found (pass --robot / ROBOT_JAR)")

    # ensure a `robot` is on PATH for the ODK Makefile
    robot_dir = tempfile.mkdtemp(prefix="robotwrap_")
    if shutil.which("robot") is None:
        wrap = Path(robot_dir) / "robot"
        wrap.write_text(f'#!/bin/bash\nexec java -Xmx12g -jar {robot} "$@"\n')
        wrap.chmod(0o755)
    print(f"owlmake: {owlmake}\nrobot:   {robot}\n")

    results = []
    print(f"{'id':<10}{'owlmake(s)':>12}{'ODK(s)':>10}{'speedup':>9}  artefact-diff")
    for arg in a.repos:
        print(f"[{arg}]", flush=True)
        r = bench_repo(arg, owlmake, robot, robot_dir, a.product, a.imp, a.runs, a.timeout)
        results.append(r)
        if r is None:
            continue
        spd = (r["odk"] / r["owlmake"]) if r["owlmake"] not in (0, float("inf")) else float("nan")
        print(f"{r['id']:<10}{r['owlmake']:>12.2f}{r['odk']:>10.2f}{spd:>8.1f}x  {status_tag(r)}")
        if r["diff"] is None:
            if r["rc_odk"] != 0:
                print(f"    ODK stderr tail: {r['err_odk'][-200:]}")
            if r["rc_owl"] != 0:
                print(f"    owlmake stderr tail: {r['err_owl'][-200:]}")
    plot(results, a.out, a.imp)


if __name__ == "__main__":
    main()
