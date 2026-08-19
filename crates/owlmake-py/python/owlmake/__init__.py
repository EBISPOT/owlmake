"""owlmake — native Python bindings for the self-contained OWL/OBO toolkit.

owlmake is one Rust library covering the whole ontology stack: format
conversion, the edit/pipeline commands, OWL reasoning, SPARQL, SSSOM and the
build itself. This package binds it natively (via pyo3 — no subprocess, no
temp files), exposing two layers:

* an in-process :class:`Ontology` — the full horned-owl object model loaded
  once into memory, with every pipeline/edit operation as a method; and
* a typed function for **every** owlmake command (``convert``, ``reason``,
  ``query``, the full ``sssom`` sub-CLI, …), plus a fluent
  :class:`Chain`, all running in-process through the same dispatch the
  ``owlmake`` binary uses.

Object model
------------
>>> from owlmake import Ontology, load, save
>>> ont = load("hp.obo")
>>> ont.add_axioms("SubClassOf(:A :B)")
>>> ont.reason("elk")
>>> for sub, sup in ont.subclass_pairs():
...     ...
>>> save(ont, "hp.owl")

Commands (one function per CLI command, kwargs == flags)
--------------------------------------------------------
>>> import owlmake
>>> owlmake.convert(input="hp.obo", output="hp.owl")            # doctest: +SKIP
>>> owlmake.sssom.convert("mappings.sssom.tsv", output="m.owl")  # doctest: +SKIP
>>> (owlmake.chain()                                            # doctest: +SKIP
...     .merge(input="edit.owl")
...     .reason(reasoner="elk")
...     .reduce()
...     .convert(output="release.owl")
...     .run())

Every command runs in-process; results come back as an :class:`OwlmakeResult`
(``returncode``/``stdout``/``stderr``), raising :class:`OwlmakeError` on a
non-zero exit unless ``raise_on_error=False``.
"""

from __future__ import annotations

import os
import shlex
import sys
from types import ModuleType
from typing import TYPE_CHECKING, Any, Dict, List, Mapping, Optional, Sequence, Union

if TYPE_CHECKING:  # imported only for type checkers; runtime stays import-free
    import pandas
    import polars

# Native object model (pyo3 extension).
from . import _owlmake
from ._owlmake import MappingSet, Ontology

# Generated, typed per-command bindings + the SSSOM sub-CLI.
from . import _runtime as _rt
from . import _sssom as sssom
from ._commands import *  # noqa: F401,F403  (generated per-command functions)
from ._commands import __all__ as _command_names
from ._runtime import Chain, OwlmakeError, OwlmakeResult, StrOrPath, version

__version__ = version()

__all__ = [
    "Ontology",
    "MappingSet",
    "load",
    "save",
    "to_pandas",
    "to_polars",
    "mapping_set_from_dataframe",
    *_command_names,
    "sssom",
    "sssom_convert",
    "dosdp",
    "chain",
    "run",
    "version",
    "Chain",
    "OwlmakeResult",
    "OwlmakeError",
    "StrOrPath",
    "__version__",
]

# Extensions owlmake infers a format from; used by load/save when no explicit
# format is given.
_EXT_FORMAT = {
    "owl": "owl",
    "rdf": "owl",
    "owx": "owx",
    "ofn": "ofn",
    "fss": "ofn",
    "obo": "obo",
    "json": "json",
    "omn": "omn",
    "ttl": "ttl",
}


def _format_for(path: Union[str, os.PathLike], fmt: Optional[str]) -> str:
    if fmt is not None:
        return fmt
    ext = os.fspath(path).rsplit(".", 1)[-1].lower()
    try:
        return _EXT_FORMAT[ext]
    except KeyError:  # pragma: no cover - error path
        raise ValueError(
            f"cannot infer ontology format from {path!r}; pass format=…"
        ) from None


def load(path: Union[str, os.PathLike], format: Optional[str] = None) -> Ontology:
    """Load an ontology from a file into an :class:`Ontology`, inferring the
    format from the extension unless ``format`` is given."""
    with open(path, "rb") as fh:
        data = fh.read()
    return Ontology.parse(data, _format_for(path, format))


def save(
    ontology: Ontology,
    path: Union[str, os.PathLike],
    format: Optional[str] = None,
) -> None:
    """Serialize an :class:`Ontology` to a file, inferring the format from the
    extension unless ``format`` is given."""
    data = ontology.serialize(_format_for(path, format))
    with open(path, "wb") as fh:
        fh.write(data)


def chain() -> Chain:
    """Start a new command :class:`Chain`.

    Commands appended to the chain share one in-memory ontology and run in a
    single in-process invocation when :meth:`Chain.run` is called.
    """
    return Chain()


Records = List[Dict[str, str]]


def _records_of(obj: Union[MappingSet, Records]) -> Records:
    """Pull ``list[dict]`` records out of a MappingSet, an Ontology query result,
    or anything already record-shaped."""
    if hasattr(obj, "records"):  # MappingSet
        return obj.records()
    return obj  # already a list of dicts (e.g. ont.query_records(...))


def to_pandas(obj: Union[MappingSet, Records]) -> "pandas.DataFrame":
    """Return a :class:`pandas.DataFrame` for a :class:`MappingSet` (its mapping
    rows) or a list of record dicts (e.g. ``ont.query_records(q)``).

    >>> to_pandas(ms)                       # doctest: +SKIP
    >>> to_pandas(ont.query_records(q))     # doctest: +SKIP
    """
    import pandas as pd  # lazy: pandas is an optional extra

    return pd.DataFrame(_records_of(obj))


def to_polars(obj: Union[MappingSet, Records]) -> "polars.DataFrame":
    """Return a :class:`polars.DataFrame` for a :class:`MappingSet` or a list of
    record dicts (the polars counterpart of :func:`to_pandas`)."""
    import polars as pl  # lazy: polars is an optional extra

    return pl.DataFrame(_records_of(obj))


def mapping_set_from_dataframe(
    df: Union["pandas.DataFrame", "polars.DataFrame", Records],
    curie_map: Optional[Mapping[str, str]] = None,
) -> MappingSet:
    """Build a :class:`MappingSet` from a pandas **or** polars DataFrame (or any
    object with ``to_dict("records")`` / ``to_dicts()``), with an optional CURIE
    map. Missing/NaN cells are dropped.

    >>> ms = mapping_set_from_dataframe(df, curie_map={"X": "http://ex/x/"})  # doctest: +SKIP
    """
    if hasattr(df, "to_dicts"):  # polars
        records = df.to_dicts()
    elif hasattr(df, "to_dict"):  # pandas
        records = df.to_dict("records")
    else:
        records = list(df)
    # Stringify and drop null/NaN so the native side sees clean str->str rows.
    clean = [
        {str(k): str(v) for k, v in row.items() if v is not None and v == v}
        for row in records
    ]
    return MappingSet.from_records(clean, dict(curie_map) if curie_map else None)


def dosdp(
    pattern_yaml: str,
    data: Union[str, Records, "pandas.DataFrame", "polars.DataFrame"],
) -> Ontology:
    """Generate an :class:`Ontology` from a DOSDP pattern (YAML text) and a data
    table, in memory.

    ``data`` may be TSV/CSV text, a list of ``{column: value}`` row dicts, or a
    pandas/polars DataFrame:

    >>> ont = owlmake.dosdp(pattern_yaml, df)            # doctest: +SKIP

    The file/flag CLI form remains available via ``owlmake.run("dosdp", ...)``.
    """
    return _owlmake.dosdp(pattern_yaml, data)


def sssom_convert(input: str, to: str, from_format: str = "tsv") -> str:
    """Convert a SSSOM mapping set between serializations, in memory.

    ``input`` is the mapping-set text; ``to`` / ``from_format`` are format names
    (in: ``tsv``/``csv``/``json``/``obographs``/``alignment``; out:
    ``tsv``/``csv``/``json``/``ttl``/``owl``). String → string, no files. For
    the file-based forms and the other sssom subcommands, use the ``sssom``
    sub-CLI (``owlmake.sssom.convert(...)``) or ``owlmake.run("sssom", ...)``.
    """
    return _owlmake.sssom_convert(input, to, from_format)


def _normalize_argv(args: Sequence[StrOrPath]) -> list:
    """Turn the variadic ``run``/callable arguments into an argv list.

    * A single string is split shell-style (``"reason -i a.owl -o b.owl"``),
      so a whole command line can be passed as one argument.
    * Multiple arguments are taken as already-split tokens (path-like values
      are accepted and stringified).
    * A leading ``robot`` token is dropped, so ``"robot reason …"`` runs as
      ``"reason …"``: owlmake's own CLI accepts that token as a no-op prefix on
      any command line, and normalizing it here keeps the two entry points
      dispatching the same argv.
    """
    if len(args) == 1 and isinstance(args[0], str):
        toks = shlex.split(args[0])
    else:
        toks = [a if isinstance(a, str) else _rt._fspath(a) for a in args]
    if toks and toks[0] == "robot":
        toks = toks[1:]
    return toks


def run(
    *args: StrOrPath,
    cwd: Optional[StrOrPath] = None,
    env: Optional[Mapping[str, str]] = None,
    capture: bool = True,
    raise_on_error: bool = True,
) -> OwlmakeResult:
    """Run owlmake with an arbitrary command in-process — the escape hatch for
    anything the typed API does not cover (raw chains, new flags, orchestration
    commands like ``make``, etc.).

    Accepts either a whole command line as one string or pre-split tokens, and
    ignores a leading ``robot``:

    >>> owlmake.run("reason -i a.obo -o b.owl")                  # doctest: +SKIP
    >>> owlmake.run("convert", "-i", "a.obo", "-o", "a.owl")     # doctest: +SKIP
    >>> owlmake.run("robot", "reason", "-i", "a.owl")            # doctest: +SKIP

    The package itself is callable as a shorthand for this function:

    >>> import owlmake; owlmake("reason -i a.obo -o b.owl")      # doctest: +SKIP
    """
    return _rt.run_raw(
        _normalize_argv(args),
        cwd=cwd, env=env, capture=capture, raise_on_error=raise_on_error,
    )


class _CallableModule(ModuleType):
    """Makes the ``owlmake`` module itself callable, so ``owlmake("reason …")``
    is shorthand for :func:`owlmake.run`."""

    def __call__(self, *args: StrOrPath, **run_opts) -> OwlmakeResult:
        return run(*args, **run_opts)


# Swap in the callable module type (a no-op for attribute access / imports).
sys.modules[__name__].__class__ = _CallableModule
