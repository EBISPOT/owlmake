#!/usr/bin/env python3
"""Generate the owlmake Python command bindings from the authoritative CLI spec.

Pipeline:

1. Obtain the clap-introspected command/flag tree, in order of preference:
   a JSON file passed as ``argv[1]``; else the installed native extension
   (``owlmake._owlmake.cli_spec()``); else ``owlmake __cli-spec`` from a freshly
   built binary under ``target/``. All three are the same data — produced by
   introspecting the one ``clap::Command`` tree (see ``src/cli.rs``).
2. Merge in the hand-authored description of the ``sssom`` sub-CLI (parsed by
   hand rather than by clap, so its grammar — positional inputs and dynamic
   ``--<slot>`` options — is not in the clap tree) and a note for ``jq``.
3. Write the merged spec to two places:
     * ``crates/owlmake-py/owlmake_cli_spec.json`` — the checked-in *coverage
       artifact* you read through to confirm every command and flag is covered.
     * ``python/owlmake/_spec.json``                — packaged data the runtime
       renders argv from.
4. Generate ``python/owlmake/_commands.py`` (one typed function + one fluent
   :class:`Chain` method per command) and ``python/owlmake/_sssom.py`` (one
   typed function per sssom subcommand).

The generated wrappers all execute **in-process** through the native extension
(``owlmake._owlmake.cli``); there is no subprocess. Re-run this whenever the
Rust CLI changes; the output is fully derived.
"""

from __future__ import annotations

import json
import keyword
import subprocess
import sys
from pathlib import Path
from typing import Any, Dict, List

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent
PKG = CRATE / "python" / "owlmake"
REPO = CRATE.parent.parent

RUN_KEYS = ("binary", "cwd", "env", "capture", "raise_on_error", "timeout")
RESERVED = set(RUN_KEYS) | {"self"}

# Execution-control parameters appended to every generated function (module
# functions; Chain methods omit them since they don't execute).
RUN_PARAMS = (
    "binary: Optional[StrOrPath] = None",
    "cwd: Optional[StrOrPath] = None",
    "env: Optional[Mapping[str, str]] = None",
    "capture: bool = True",
    "raise_on_error: bool = True",
    "timeout: Optional[float] = None",
)
RUN_FORWARD = ", ".join(f"{k}={k}" for k in RUN_KEYS)


# --------------------------------------------------------------------------- #
# Hand-authored sssom sub-CLI description (mirrors src/sssom/cli.rs exactly).
# --------------------------------------------------------------------------- #
def _opt(name, long, short=None, nargs=1, multiple=False, help=""):
    return {"name": name, "long": long, "short": short, "nargs": nargs,
            "multiple": multiple, "help": help}


def _flag(name, true, false=None, default=None, help=""):
    return {"name": name, "true": true, "false": false, "default": default, "help": help}


_PROPAGATE = _flag("propagate", "--propagate", "--no-propagate", True,
                   "Propagate set-level slots onto each mapping row.")
_CONDENSE = _flag("condense", "--condense", "--no-condense", True,
                  "Condense multi-valued slots in the output.")
_OUTPUT = _opt("output", "--output", "-o", help="Output path (default: stdout).")

SSSOM_SUBCOMMANDS: List[Dict[str, Any]] = [
    {
        "name": "convert", "supported": True,
        "help": "Convert a mapping set to another serialization (TSV/CSV/JSON/RDF/OWL).",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT, _opt("output_format", "--output-format", "-O",
                                  help="Output format (tsv/csv/json/owl/ttl/...).")],
        "flags": [_PROPAGATE, _CONDENSE],
    },
    {
        "name": "parse", "supported": True,
        "help": "Parse a mapping set, apply defaults/cleaning, and re-serialize as SSSOM TSV.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [
            _opt("input_format", "--input-format", "-I", help="Input format override."),
            _opt("metadata", "--metadata", "-m", help="External metadata YAML."),
            _OUTPUT,
            _opt("mapping_predicate_filter", "--mapping-predicate-filter", "-F",
                 multiple=True, help="Keep only these predicate ids (repeatable)."),
            _opt("prefix_map_mode", "--prefix-map-mode", "-C", help="Prefix-map handling mode."),
        ],
        "flags": [
            _PROPAGATE, _CONDENSE,
            _flag("clean_prefixes", "--clean-prefixes", "--no-clean-prefixes", True,
                  "Drop unused curie_map entries.", ),
            _flag("strict_clean_prefixes", "--strict-clean-prefixes",
                  "--no-strict-clean-prefixes", True,
                  "Error on CURIEs whose prefix is undeclared."),
            _flag("embedded_mode", "--embedded-mode", "--non-embedded-mode", True,
                  "Embed metadata in the TSV (vs sidecar)."),
        ],
    },
    {
        "name": "validate", "supported": True,
        "help": "Validate a mapping set (JsonSchema / PrefixMapCompleteness / StrictCurieFormat).",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_opt("validation_types", "--validation-types", "-V", multiple=True,
                         help="Validation types to run (repeatable).")],
        "flags": [_PROPAGATE],
    },
    {
        "name": "split", "supported": True,
        "help": "Split a mapping set into one file per (subject,predicate,object) prefix group.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_opt("output_directory", "--output-directory", "-d",
                         help="Directory for the split files."),
                    _opt("method", "--method", help="Splitting method.")],
        "flags": [],
    },
    {
        "name": "dedupe", "supported": True,
        "help": "Remove duplicate mappings, keeping the highest-confidence row per key.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT], "flags": [],
    },
    {
        "name": "diff", "supported": True,
        "help": "Diff two mapping sets by (subject,predicate,object).",
        "positionals": [{"name": "left", "required": True, "variadic": False,
                         "help": "First mapping set."},
                        {"name": "right", "required": True, "variadic": False,
                         "help": "Second mapping set."}],
        "options": [_OUTPUT], "flags": [],
    },
    {
        "name": "merge", "supported": True,
        "help": "Merge several mapping sets into one (optionally reconciling redundancy).",
        "positionals": [{"name": "inputs", "required": True, "variadic": True,
                         "help": "Input mapping sets."}],
        "options": [_OUTPUT],
        "flags": [_PROPAGATE, _CONDENSE,
                  _flag("reconcile", "--reconcile", default=False,
                        help="Drop lower-confidence redundant rows.")],
    },
    {
        "name": "sort", "supported": True,
        "help": "Sort a mapping set by columns and/or rows.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT],
        "flags": [_flag("by_columns", "--by-columns", default=False,
                        help="Sort columns into canonical order."),
                  _flag("by_rows", "--by-rows", default=False,
                        help="Sort rows.")],
    },
    {
        "name": "filter", "supported": True,
        "help": "Filter mappings by one or more mapping-slot constraints (glob `*` supported).",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT], "flags": [], "dynamic_slots": True,
    },
    {
        "name": "annotate", "supported": True,
        "help": "Set or append mapping-set metadata slots.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT],
        "flags": [_flag("replace_multivalued", "--replace-multivalued", default=False,
                        help="Replace rather than append multi-valued slots.")],
        "dynamic_slots": True,
    },
    {
        "name": "remove", "supported": True,
        "help": "Remove from a mapping set every mapping present in another set.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT, _opt("remove_map", "--remove-map",
                                  help="Mapping set whose rows to remove (required).")],
        "flags": [],
    },
    {
        "name": "invert", "supported": True,
        "help": "Invert mapping predicates (and subject/object), optionally merging with the input.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT, _opt("subject_prefix", "--subject-prefix", "-P",
                                  help="Restrict to this subject prefix."),
                    _opt("inverse_map", "--inverse-map", help="Custom inverse-predicate map.")],
        "flags": [_flag("merge_inverted", "--merge-inverted", "--no-merge-inverted", True,
                        "Append inverted rows to the originals."),
                  _flag("update_justification", "--update-justification",
                        "--no-update-justification", True,
                        "Set mapping_justification to MappingInversion.")],
    },
    {
        "name": "reconcile-prefixes", "supported": True,
        "help": "Rewrite CURIEs/curie_map using a prefix reconciliation YAML.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT, _opt("reconcile_prefix_file", "--reconcile-prefix-file", "-p",
                                  help="Reconciliation YAML (required).")],
        "flags": [],
    },
    {
        "name": "crosstab", "supported": True,
        "help": "Tabulate a contingency table of two mapping slots.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT, _opt("fields", "--fields", "-f", nargs=2,
                                  help="Row and column slot names (two values).")],
        "flags": [_flag("transpose", "--transpose", default=False,
                        help="Transpose rows and columns.")],
    },
    {
        "name": "correlations", "supported": True,
        "help": "Alias of `crosstab` (same tabulation engine).",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT, _opt("fields", "--fields", "-f", nargs=2,
                                  help="Row and column slot names (two values).")],
        "flags": [_flag("transpose", "--transpose", default=False,
                        help="Transpose rows and columns.")],
    },
    {
        "name": "partition", "supported": True,
        "help": "Partition merged mappings into connected-component cliques.",
        "positionals": [{"name": "inputs", "required": True, "variadic": True,
                         "help": "Input mapping sets."}],
        "options": [_opt("output_directory", "--output-directory", "-d",
                         help="Directory for the partition files.")],
        "flags": [],
    },
    {
        "name": "cliquesummary", "supported": True,
        "help": "Summarize mapping cliques (id, size, members).",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input mapping set."}],
        "options": [_OUTPUT, _opt("metadata", "--metadata", "-m", help="External metadata."),
                    _opt("statsfile", "--statsfile", "-s", help="Stats output file.")],
        "flags": [],
    },
    {
        "name": "rewire", "supported": True,
        "help": "Rewrite an ontology, collapsing entities linked by equivalence mappings.",
        "positionals": [{"name": "input", "required": True, "variadic": False,
                         "help": "Input ontology."}],
        "options": [_OUTPUT,
                    _opt("mapping_file", "--mapping-file", "-m",
                         help="SSSOM mapping set (required)."),
                    _opt("input_format", "--input-format", "-I", help="Ontology input format."),
                    _opt("output_format", "--output-format", "-O", help="Ontology output format."),
                    _opt("precedence", "--precedence", multiple=True,
                         help="Prefix precedence for ambiguous targets (repeatable).")],
        "flags": [],
    },
    {
        "name": "xref-extract", "supported": True,
        "help": "Extract oboInOwl:hasDbXref annotations into a SSSOM mapping set.",
        "positionals": [{"name": "input", "required": False, "variadic": False,
                         "help": "Input ontology (accepted positionally or as --input)."}],
        "options": [_opt("output", "--output", "-o", help="Output path."),
                    _opt("mapping_file", "--mapping-file", help="Output mapping file."),
                    _opt("map_prefix_to_predicate", "--map-prefix-to-predicate", multiple=True,
                         help="'PREFIX PREDICATE' assignment (repeatable).")],
        "flags": [_flag("all_xrefs", "--all-xrefs", default=False,
                        help="Extract xrefs of every prefix."),
                  _flag("drop_duplicates", "--drop-duplicates", default=False,
                        help="Drop duplicate (s,p,o) rows.")],
    },
    # Recognized by the binary but not yet implemented (parse args, report clearly).
    {"name": "ptable", "supported": False, "help": "Recognized but not yet implemented in owlmake.",
     "positionals": [{"name": "inputs", "required": False, "variadic": True, "help": "Inputs."}],
     "options": [], "flags": []},
    {"name": "dosql", "supported": False, "help": "Recognized but not yet implemented in owlmake.",
     "positionals": [{"name": "inputs", "required": False, "variadic": True, "help": "Inputs."}],
     "options": [], "flags": []},
    {"name": "sparql", "supported": False, "help": "Recognized but not yet implemented in owlmake.",
     "positionals": [{"name": "inputs", "required": False, "variadic": True, "help": "Inputs."}],
     "options": [], "flags": []},
    {"name": "serve-rdf", "supported": False, "help": "Recognized but not yet implemented in owlmake.",
     "positionals": [{"name": "inputs", "required": False, "variadic": True, "help": "Inputs."}],
     "options": [], "flags": []},
]


# --------------------------------------------------------------------------- #
# Helpers
# --------------------------------------------------------------------------- #
def safe_ident(name: str) -> str:
    ident = name.replace("-", "_")
    if keyword.iskeyword(ident) or ident in RESERVED:
        ident += "_"
    return ident


def unique_ident(name: str, seen: set) -> str:
    """Like :func:`safe_ident` but guarantees uniqueness within one signature
    (suffixing ``_`` on collision), so two flags can never share a parameter."""
    ident = safe_ident(name)
    while ident in seen:
        ident += "_"
    seen.add(ident)
    return ident


def doc_escape(text: str) -> str:
    return text.replace("\\", "\\\\").replace('"""', '\\"\\"\\"')


def arg_param(arg: Dict[str, Any]):
    """Return (pyname, annotation, default, arg_id) for a clap arg."""
    longs = arg.get("longs") or []
    base = longs[0] if longs else arg["id"]
    pyname = safe_ident(base)
    action = arg["action"]
    possible = set(arg.get("possible_values") or [])
    variadic = bool(arg.get("variadic"))
    max_values = int(arg.get("max_values", 1))

    if action in ("set_true", "set_false"):
        return pyname, "bool", "False", arg["id"]
    if action == "count":
        return pyname, "int", "0", arg["id"]
    if possible == {"true", "false"}:
        return pyname, "Optional[bool]", "None", arg["id"]
    if action == "append":
        # Repeatable flag. A fixed multi-value group (e.g. --query-pair FILE
        # OUTPUT) takes a sequence of groups; otherwise a flat sequence (a bare
        # scalar is also accepted at runtime and wrapped).
        if max_values > 1 and not variadic:
            return pyname, "Optional[Sequence[Sequence[StrOrPath]]]", "None", arg["id"]
        return pyname, "Optional[Sequence[StrOrPath]]", "None", arg["id"]
    if variadic or max_values > 1:
        return pyname, "Optional[Sequence[StrOrPath]]", "None", arg["id"]
    return pyname, "Optional[StrOrPath]", "None", arg["id"]


def arg_doc(arg: Dict[str, Any], pyname: str) -> str:
    bits = []
    flags = " / ".join([f"--{l}" for l in (arg.get("longs") or [])]
                       + [f"-{s}" for s in (arg.get("shorts") or [])])
    if flags:
        bits.append(f"({flags})")
    if arg.get("required"):
        bits.append("[required]")
    pv = [p for p in (arg.get("possible_values") or []) if p not in ("true", "false")]
    if pv:
        bits.append("choices: " + ", ".join(pv))
    help_text = (arg.get("help") or "").strip().replace("\n", " ")
    line = f"    {pyname}: " + " ".join(bits)
    if help_text:
        line += (" " if bits else "") + help_text
    return line.rstrip()


def render_command_function(cmd: Dict[str, Any]) -> str:
    name = cmd["name"]
    pyfn = safe_ident(name)
    args = [a for a in cmd["args"] if not a.get("hidden")]
    params, values, docs = [], [], []
    seen: set = set(RUN_KEYS)
    for a in args:
        pyname, ann, default, arg_id = arg_param(a)
        pyname = unique_ident(pyname, seen)
        params.append(f"{pyname}: {ann} = {default}")
        values.append(f'        "{arg_id}": {pyname},')
        docs.append(arg_doc(a, pyname))

    about = (cmd.get("about") or "").strip()
    doc = '"""' + doc_escape(about) + "\n"
    if cmd.get("aliases"):
        doc += "\n    Aliases: " + ", ".join(cmd["aliases"]) + "\n"
    if docs:
        doc += "\n    Flags:\n" + "\n".join(doc_escape(d) for d in docs) + "\n"
    doc += '    """'

    sig_params = ", ".join(["*"] + params + list(RUN_PARAMS))
    chain_params = ", ".join(["self", "*"] + params) if params else "self"
    valblock = "\n".join(values)
    # Both functions are top-level `def`s (body at 4 spaces), so the same dict
    # literal indentation works for the module function and the Chain method.
    valdict = "{\n" + valblock + "\n    }" if values else "{}"

    fn = (
        f"def {pyfn}({sig_params}) -> OwlmakeResult:\n"
        f"    {doc}\n"
        f"    return _rt.run_command({name!r}, {valdict}, {RUN_FORWARD})\n\n\n"
    )
    chain = (
        f"def _chain_{pyfn}({chain_params}) -> 'Chain':\n"
        f"    {doc}\n"
        f"    return self._add({name!r}, {valdict})\n\n\n"
        f"Chain.{pyfn} = _chain_{pyfn}\n\n\n"
    )
    return fn + chain


def render_sssom_function(sub: Dict[str, Any]) -> str:
    name = sub["name"]
    pyfn = safe_ident(name)
    positionals = sub.get("positionals", [])
    options = sub.get("options", [])
    flags = sub.get("flags", [])
    dynamic = sub.get("dynamic_slots", False)

    pos_params, value_keys, docs = [], [], []
    seen: set = set(RUN_KEYS) | {"slots"}
    for p in positionals:
        pid = unique_ident(p["name"], seen)
        if p.get("variadic"):
            pos_params.append(f"*{pid}: StrOrPath")
        else:
            pos_params.append(f"{pid}: Optional[StrOrPath] = None")
        value_keys.append(f'        "{p["name"]}": {pid},')
        req = " [required]" if p.get("required") else ""
        docs.append(f"    {pid}:{req} {p.get('help', '')}".rstrip())

    kw_params = []
    for o in options:
        oid = unique_ident(o["name"], seen)
        ann = "Optional[Sequence[StrOrPath]]" if o.get("multiple") or o.get("nargs", 1) > 1 \
            else "Optional[StrOrPath]"
        kw_params.append(f"{oid}: {ann} = None")
        value_keys.append(f'        "{o["name"]}": {oid},')
        spell = " / ".join([s for s in (o.get("long"), o.get("short")) if s])
        docs.append(f"    {oid}: ({spell}) {o.get('help', '')}".rstrip())
    for f in flags:
        fid = unique_ident(f["name"], seen)
        kw_params.append(f"{fid}: Optional[bool] = None")
        value_keys.append(f'        "{f["name"]}": {fid},')
        spell = f["true"] + (f" / {f['false']}" if f.get("false") else "")
        dflt = f" (default {f['default']})" if f.get("default") is not None else ""
        docs.append(f"    {fid}: ({spell}){dflt} {f.get('help', '')}".rstrip())

    # Build the signature: positionals, then `*`, kw options/flags, dynamic
    # slots, then run options. If a variadic positional is present it already
    # supplies the `*`, so don't add a bare one.
    has_var = any(p.get("variadic") for p in positionals)
    head = list(pos_params)
    tail = list(kw_params)
    if dynamic:
        # filter/annotate accept one option per arbitrary mapping slot; expose
        # them as an explicit dict so the run-control kwargs still work (a
        # `**slots` var-keyword would have to be the final parameter).
        tail.append("slots: Optional[Mapping[str, StrOrPath]] = None")
        slots_forward = "slots or {}"
    else:
        slots_forward = "{}"
    run_tail = list(RUN_PARAMS)

    if has_var:
        sig = ", ".join(head + tail + run_tail)
    else:
        sig = ", ".join(head + (["*"] if tail or run_tail else []) + tail + run_tail)

    about = (sub.get("help") or "").strip()
    if not sub.get("supported", True):
        about += "\n\n    NOTE: the arguments are parsed and the call reports the gap; nothing is computed."
    doc = '"""' + doc_escape(about) + "\n"
    if docs:
        doc += "\n    Arguments:\n" + "\n".join(doc_escape(d) for d in docs) + "\n"
    doc += '    """'

    valblock = "\n".join(value_keys)
    valdict = "{\n" + valblock + "\n    }" if value_keys else "{}"

    return (
        f"def {pyfn}({sig}) -> OwlmakeResult:\n"
        f"    {doc}\n"
        f"    return _rt.run_sssom({name!r}, {valdict}, {slots_forward}, {RUN_FORWARD})\n\n\n"
    )


HEADER = '''\
# AUTO-GENERATED by scripts/generate.py from the owlmake CLI spec.
# Do not edit by hand: re-run the generator after changing the Rust CLI.
"""{title}"""

from __future__ import annotations

from typing import Mapping, Optional, Sequence

from . import _runtime as _rt
from ._runtime import Chain, OwlmakeResult, StrOrPath

'''


# --------------------------------------------------------------------------- #
# Spec acquisition
# --------------------------------------------------------------------------- #
def load_clap_spec() -> Dict[str, Any]:
    """Return the clap CLI spec dict, from argv[1] / the installed extension /
    a built binary, in that order."""
    if len(sys.argv) > 1:
        return json.loads(Path(sys.argv[1]).read_text())
    try:
        from owlmake._owlmake import cli_spec  # type: ignore
        return json.loads(cli_spec())
    except Exception:
        pass
    for build in ("release", "debug"):
        exe = REPO / "target" / build / "owlmake"
        if exe.exists():
            out = subprocess.run([str(exe), "__cli-spec"], capture_output=True,
                                 text=True, check=True)
            return json.loads(out.stdout)
    raise SystemExit(
        "could not obtain the CLI spec: pass a spec JSON file as argv[1], install "
        "the owlmake extension, or build the binary (cargo build)."
    )


def main() -> int:
    clap = load_clap_spec()

    # Merge sssom + jq into the full spec.
    spec = dict(clap)
    spec["sssom"] = {
        "note": "owlmake's `sssom` sub-CLI: positional inputs plus dynamic `--<slot>` options, parsed by hand rather than by clap.",
        "subcommands": SSSOM_SUBCOMMANDS,
    }
    spec["jq"] = {
        "note": "Bundled pure-Rust jq engine; CLI-only (owlmake jq / owlmake.run(\"jq\", ...)), no dedicated binding.",
        "passthrough": True,
    }

    # Write the coverage artifact + packaged data.
    text = json.dumps(spec, indent=2)
    (CRATE / "owlmake_cli_spec.json").write_text(text + "\n")
    (PKG / "_spec.json").write_text(text + "\n")

    # Generate the command bindings.
    body = HEADER.format(
        title="Typed owlmake command bindings (one function per command, plus Chain).")
    exports = []
    for cmd in spec["commands"]:
        if cmd.get("passthrough"):
            continue  # jq / sssom handled separately
        body += render_command_function(cmd)
        exports.append(safe_ident(cmd["name"]))
    body += "__all__ = [\n" + "".join(f"    {e!r},\n" for e in sorted(exports)) + "]\n"
    (PKG / "_commands.py").write_text(body)

    # Generate the sssom bindings.
    sbody = HEADER.format(title="Typed bindings for the owlmake `sssom` sub-CLI.")
    sexports = []
    for sub in SSSOM_SUBCOMMANDS:
        sbody += render_sssom_function(sub)
        sexports.append(safe_ident(sub["name"]))
    sbody += "__all__ = [\n" + "".join(f"    {e!r},\n" for e in sorted(sexports)) + "]\n"
    (PKG / "_sssom.py").write_text(sbody)

    print(f"generated: {len(exports)} commands, {len(sexports)} sssom subcommands")
    print(f"spec: {CRATE / 'owlmake_cli_spec.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
