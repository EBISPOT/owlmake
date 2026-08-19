#!/usr/bin/env python3
"""Generate the owlmake CLI reference (Markdown) from the authoritative CLI spec.

The spec is the same clap introspection the language bindings are generated from
(`om __cli-spec`; see src/cli.rs::dump_cli_spec), so the docs can never
drift from the real commands and flags. Source, in order of preference: a JSON
file passed as argv[1]; else `om __cli-spec` from a built binary under
target/ (or $OWLMAKE_BIN). Output: docs/cli.md (or argv[2]).
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Standalone sub-CLIs intercepted before clap; their full surface has its own
# `--help`, so the reference points at it rather than an opaque placeholder.
PASSTHROUGH_NOTE = {
    "jq": "Bundled pure-Rust jq engine. Accepts the usual jq flags/filter/files; "
    "run `om jq --help`.",
    "sssom": "Drop-in SSSOM CLI (convert/parse/validate/merge/sort/filter/…). "
    "Run `om sssom --help` for its subcommands.",
    "sed": "Bundled GNU-compatible `sed`. Run `om sed --help`.",
    "grep": "ripgrep-backed `grep`. Run `om grep --help`.",
    "comm": "Bundled `comm`. Run `om comm --help`.",
}


def load_spec() -> dict:
    if len(sys.argv) > 1 and sys.argv[1]:
        return json.loads(Path(sys.argv[1]).read_text())
    exe = os.environ.get("OWLMAKE_BIN")
    cands = [exe] if exe else []
    cands += [REPO / "target" / b / "om" for b in ("release", "debug")]
    for c in cands:
        if c and Path(c).exists():
            out = subprocess.run([str(c), "__cli-spec"], capture_output=True, text=True, check=True)
            return json.loads(out.stdout)
    raise SystemExit("no CLI spec: pass a spec file, set OWLMAKE_BIN, or `cargo build`")


def flag_cell(arg: dict) -> str:
    names = ["`--{}`".format(x) for x in (arg.get("longs") or [])]
    names += ["`-{}`".format(x) for x in (arg.get("shorts") or [])]
    vn = " ".join(arg.get("value_names") or [])
    sig = ", ".join(names)
    if vn and arg["action"] in ("set", "append"):
        sig += f" `{vn}`"
        if arg.get("variadic"):
            sig += "…"
    return sig


def md_escape(s: str) -> str:
    return (s or "").replace("\n", " ").replace("|", "\\|").strip()


def render(spec: dict) -> str:
    out: list[str] = []
    out.append(f"# {spec['program']} CLI reference\n")
    if spec.get("about"):
        out.append(md_escape(spec["about"]) + "\n")
    out.append(f"_Version {spec.get('version', '?')}. Auto-generated from "
               "`owlmake __cli-spec` — do not edit by hand._\n")
    out.append(
        "owlmake commands chain like ROBOT, threading one in-memory ontology: "
        "`owlmake merge -i a.owl reason reduce -o out.owl`. Run "
        "`owlmake <command> --help` for any command.\n"
    )

    cmds = sorted(spec["commands"], key=lambda c: c["name"])
    out.append("## Commands\n")
    out.append("| Command | Description |")
    out.append("| --- | --- |")
    for c in cmds:
        out.append(f"| [`{c['name']}`](#{c['name']}) | {md_escape(c.get('about'))} |")
    out.append("")

    for c in cmds:
        out.append(f"### {c['name']}\n")
        if c.get("about"):
            out.append(md_escape(c["about"]) + "\n")
        if c.get("aliases"):
            out.append("Aliases: " + ", ".join(f"`{a}`" for a in c["aliases"]) + "\n")
        if c.get("passthrough"):
            out.append(PASSTHROUGH_NOTE.get(c["name"], "Passthrough command.") + "\n")
            continue
        args = [a for a in c.get("args", []) if not a.get("hidden")]
        if not args:
            out.append("_No options._\n")
            continue
        out.append("| Flag | Req | Choices | Default | Description |")
        out.append("| --- | --- | --- | --- | --- |")
        for a in args:
            req = "yes" if a.get("required") else ""
            choices = ", ".join(f"`{p}`" for p in (a.get("possible_values") or []))
            default = ", ".join(f"`{d}`" for d in (a.get("defaults") or []))
            out.append(
                f"| {flag_cell(a)} | {req} | {choices} | {default} | {md_escape(a.get('help'))} |"
            )
        out.append("")
    return "\n".join(out) + "\n"


def main() -> int:
    spec = load_spec()
    dest = Path(sys.argv[2]) if len(sys.argv) > 2 else REPO / "docs" / "cli.md"
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(render(spec))
    print(f"wrote {dest} ({len(spec['commands'])} commands)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
