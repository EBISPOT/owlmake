#!/usr/bin/env python3
"""Benchmark owlmake against ROBOT on OBO Foundry ontologies, by id.

For each ontology id it: downloads the official release (cached), classifies it
with both owlmake and ROBOT (timing the wall-clock of the `reason` command),
diffs the inferred SubClassOf closure to confirm they agree, and plots a
performance comparison.

Usage:
    scripts/obo_perf.py bspo bfo ro cl hp
    scripts/obo_perf.py --reasoner hermit bspo obi
    scripts/obo_perf.py --runs 3 --out /tmp/perf.png pato envo

Paths (override via env or flags):
    OWLMAKE_BIN   owlmake binary      (default: target/release/owlmake)
    ROBOT_JAR     robot.jar           (default: $PWD/robot.jar)
Both reasoners get the SAME input and the SAME flags (inferred direct
SubClassOf, new-ontology, no annotations), so the only differences measured are
speed and (any) classification disagreement.
"""
import argparse
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

PURL = "http://purl.obolibrary.org/obo/{id}.owl"
SUBCLASS_RE = re.compile(r"SubClassOf\(\s*(\S+)\s+(\S+?)\s*\)")
TRIVIAL_SUP = {"Thing"}
TRIVIAL_SUB = {"Nothing"}


def find_default(paths):
    for p in paths:
        if p and Path(p).exists():
            return str(p)
    return None


def localname(iri: str) -> str:
    iri = iri.strip().lstrip("<").rstrip(">")
    return re.split(r"[/#:]", iri)[-1]


def inferred_set(ofn_path: Path) -> set:
    """Normalized, non-trivial inferred SubClassOf pairs from a functional-syntax file."""
    out = set()
    text = ofn_path.read_text(errors="replace")
    for m in SUBCLASS_RE.finditer(text):
        a, b = localname(m.group(1)), localname(m.group(2))
        if a == b or b in TRIVIAL_SUP or a in TRIVIAL_SUB:
            continue
        out.add((a, b))
    return out


def download(oid: str, cache: Path) -> Path | None:
    dst = cache / f"{oid}.owl"
    if dst.exists() and dst.stat().st_size > 0:
        return dst
    url = PURL.format(id=oid)
    try:
        print(f"  downloading {url} ...", flush=True)
        with urllib.request.urlopen(url, timeout=180) as r, open(dst, "wb") as f:
            shutil.copyfileobj(r, f)
        return dst if dst.stat().st_size > 0 else None
    except Exception as e:
        print(f"  download failed: {e}")
        return None


def timed(cmd, timeout) -> tuple[float, int]:
    """Best wall-clock over the run; returns (seconds, returncode). inf on failure."""
    t0 = time.perf_counter()
    try:
        p = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           timeout=timeout)
        return time.perf_counter() - t0, p.returncode
    except subprocess.TimeoutExpired:
        return float("inf"), 124


def best_of(cmd, runs, timeout):
    best, rc = float("inf"), 1
    for _ in range(runs):
        dt, code = timed(cmd, timeout)
        rc = code
        if code == 0:
            best = min(best, dt)
    return best, rc


def bench_one(oid, owlmake, robot, reasoner, runs, timeout, work):
    f = download(oid, work / "onts")
    if f is None:
        return None
    r_out = work / "out" / f"{oid}_robot.ofn"
    o_out = work / "out" / f"{oid}_owlmake.ofn"
    robot_cmd = ["java", "-jar", robot, "reason", "--reasoner", reasoner,
                 "--input", str(f), "--axiom-generators", "subclass",
                 "--create-new-ontology", "true", "--annotate-inferred-axioms", "false",
                 "--output", str(r_out)]
    owl_cmd = [owlmake, "reason", "-r", reasoner, "-i", str(f),
               "-A", "subclass", "-n", "true", "-a", "false", "-o", str(o_out)]
    print(f"  robot ({reasoner}) ...", flush=True)
    t_robot, rc_r = best_of(robot_cmd, runs, timeout)
    print(f"  owlmake ({reasoner}) ...", flush=True)
    t_owl, rc_o = best_of(owl_cmd, runs, timeout)
    diff = None
    if rc_r == 0 and rc_o == 0:
        R, O = inferred_set(r_out), inferred_set(o_out)
        diff = (len(R - O), len(O - R), len(R), len(O))
    return dict(id=oid, mb=f.stat().st_size / 1e6, robot=t_robot, owlmake=t_owl,
                rc_robot=rc_r, rc_owl=rc_o, diff=diff)


def row_label(r):
    spd = r["robot"] / r["owlmake"] if r["owlmake"] else float("nan")
    if r["diff"] is None:
        tag = "?"
    elif r["diff"][0] == 0 and r["diff"][1] == 0:
        tag = "OK"  # inferred axioms identical
    else:
        tag = f"x{r['diff'][0]}/{r['diff'][1]}"
    return f"{r['id']}  ({r['mb']:.0f}MB, {spd:.1f}x, {tag})"


def plot_svg(ok, reasoner, out_path):
    """Dependency-free horizontal grouped bar chart, in case matplotlib is absent."""
    W, rowh, pad, left = 900, 46, 60, 320
    H = pad * 2 + rowh * len(ok)
    vmax = max(max(r["owlmake"], r["robot"]) for r in ok) or 1.0
    span = W - left - 40
    def bar(x, y, w, h, color):
        return f'<rect x="{x:.1f}" y="{y:.1f}" width="{max(w,0.5):.1f}" height="{h}" fill="{color}"/>'
    s = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="sans-serif">',
         f'<rect width="{W}" height="{H}" fill="white"/>',
         f'<text x="{W/2}" y="26" text-anchor="middle" font-size="16">owlmake vs ROBOT — reason --reasoner {reasoner} (s, lower is better)</text>']
    for i, r in enumerate(ok):
        y = pad + i * rowh
        s.append(f'<text x="10" y="{y+rowh/2+4:.0f}" font-size="13">{row_label(r)}</text>')
        s.append(bar(left, y + 4, span * r["owlmake"] / vmax, rowh / 2 - 5, "#2c7fb8"))
        s.append(bar(left, y + rowh / 2 + 1, span * r["robot"] / vmax, rowh / 2 - 5, "#d95f0e"))
        s.append(f'<text x="{left+span*r["owlmake"]/vmax+5:.0f}" y="{y+rowh/2:.0f}" font-size="11">{r["owlmake"]:.2f}s owlmake</text>')
        s.append(f'<text x="{left+span*r["robot"]/vmax+5:.0f}" y="{y+rowh-4:.0f}" font-size="11">{r["robot"]:.2f}s ROBOT</text>')
    s.append("</svg>")
    Path(out_path).write_text("\n".join(s))
    print(f"\nplot written to {out_path}  (matplotlib unavailable; wrote SVG)")


def plot(results, reasoner, out_png):
    ok = [r for r in results if r and r["robot"] != float("inf") and r["owlmake"] != float("inf")]
    if not ok:
        print("no successful runs to plot")
        return
    ok.sort(key=lambda r: r["owlmake"])
    try:
        import matplotlib
        matplotlib.use("Agg")  # headless
        import matplotlib.pyplot as plt
    except ImportError:
        plot_svg(ok, reasoner, str(Path(out_png).with_suffix(".svg")))
        return
    ids = [r["id"] for r in ok]
    y = range(len(ids))
    h = 0.38
    fig, ax = plt.subplots(figsize=(9, max(3, 0.6 * len(ids) + 1.5)))
    ax.barh([i + h / 2 for i in y], [r["owlmake"] for r in ok], height=h,
            label="owlmake", color="#2c7fb8")
    ax.barh([i - h / 2 for i in y], [r["robot"] for r in ok], height=h,
            label="ROBOT", color="#d95f0e")
    ax.set_yticks(list(y))
    labels = []
    for r in ok:
        spd = r["robot"] / r["owlmake"] if r["owlmake"] else float("nan")
        if r["diff"] is None:
            tag = "?"
        elif r["diff"][0] == 0 and r["diff"][1] == 0:
            tag = "✓"  # identical
        else:
            tag = f"✗{r['diff'][0]}/{r['diff'][1]}"
        labels.append(f"{r['id']}  ({r['mb']:.0f}MB, {spd:.1f}×, {tag})")
    ax.set_yticklabels(labels)
    ax.set_xlabel("reason wall-clock (s, best of runs; lower is better)")
    ax.set_title(f"owlmake vs ROBOT — `reason --reasoner {reasoner}` on OBO Foundry\n"
                 f"✓ = inferred axioms identical; ×a/b = onlyROBOT/onlyOwlmake; N× = speedup")
    ax.legend(loc="lower right")
    ax.grid(axis="x", alpha=0.3)
    fig.tight_layout()
    fig.savefig(out_png, dpi=130)
    print(f"\nplot written to {out_png}")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("ids", nargs="+", help="OBO Foundry ontology ids (e.g. bspo cl hp)")
    ap.add_argument("--reasoner", default="elk", help="elk|hermit|... (same for both; default elk)")
    ap.add_argument("--runs", type=int, default=1, help="runs per tool, best wall-clock taken")
    ap.add_argument("--timeout", type=int, default=900, help="per-run timeout (s)")
    ap.add_argument("--owlmake", default=os.environ.get("OWLMAKE_BIN"))
    ap.add_argument("--robot", default=os.environ.get("ROBOT_JAR"))
    ap.add_argument("--work", default="/tmp/obo_perf", help="cache + output dir")
    ap.add_argument("--out", default=None, help="output PNG (default: <work>/obo_perf_<reasoner>.png)")
    a = ap.parse_args()

    repo = Path(__file__).resolve().parent.parent
    owlmake = find_default([a.owlmake, repo / "target/release/owlmake", repo / "target/profiling/owlmake"])
    robot = find_default([a.robot, repo / "robot.jar", shutil.which("robot")])
    if not owlmake:
        sys.exit("owlmake binary not found (build it or pass --owlmake / OWLMAKE_BIN)")
    if not robot:
        sys.exit("robot.jar not found (pass --robot / ROBOT_JAR)")
    print(f"owlmake: {owlmake}\nrobot:   {robot}\nreasoner: {a.reasoner}\n")

    work = Path(a.work)
    (work / "onts").mkdir(parents=True, exist_ok=True)
    (work / "out").mkdir(parents=True, exist_ok=True)
    out_png = a.out or str(work / f"obo_perf_{a.reasoner}.png")

    results = []
    print(f"{'id':<10}{'MB':>6}{'owlmake(s)':>12}{'robot(s)':>10}{'speedup':>9}  diff")
    for oid in a.ids:
        print(f"[{oid}]", flush=True)
        r = bench_one(oid, owlmake, robot, a.reasoner, a.runs, a.timeout, work)
        results.append(r)
        if r is None:
            print(f"{oid:<10}  DOWNLOAD_FAIL")
            continue
        spd = (r["robot"] / r["owlmake"]) if r["owlmake"] not in (0, float("inf")) else float("nan")
        if r["diff"] is None:
            d = f"reason failed (r={r['rc_robot']},o={r['rc_owl']})"
        elif r["diff"][0] == 0 and r["diff"][1] == 0:
            d = f"IDENTICAL ({r['diff'][2]} axioms)"
        else:
            d = f"DIFF onlyROBOT={r['diff'][0]} onlyOwlmake={r['diff'][1]}"
        print(f"{r['id']:<10}{r['mb']:>6.0f}{r['owlmake']:>12.2f}{r['robot']:>10.2f}{spd:>8.1f}x  {d}")

    # write TSV
    tsv = work / f"obo_perf_{a.reasoner}.tsv"
    with open(tsv, "w") as fh:
        fh.write("id\tMB\towlmake_s\trobot_s\tspeedup\tonlyROBOT\tonlyOwlmake\tinferred\n")
        for r in results:
            if not r:
                continue
            spd = (r["robot"] / r["owlmake"]) if r["owlmake"] not in (0, float("inf")) else ""
            d = r["diff"] or ("", "", "", "")
            fh.write(f"{r['id']}\t{r['mb']:.1f}\t{r['owlmake']:.3f}\t{r['robot']:.3f}\t"
                     f"{spd if spd=='' else f'{spd:.2f}'}\t{d[0]}\t{d[1]}\t{d[3]}\n")
    print(f"\ntable written to {tsv}")
    plot(results, a.reasoner, out_png)


if __name__ == "__main__":
    main()
