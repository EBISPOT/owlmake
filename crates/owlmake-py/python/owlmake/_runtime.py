"""Runtime support shared by every generated owlmake binding.

This module is hand-written and stable; the per-command wrappers in
``_commands.py`` and ``_sssom.py`` are generated from ``_spec.json`` (see
``scripts/generate.py``) and call into the helpers here. It handles two things:

* turning typed keyword arguments into the exact argv the CLI expects (driven
  entirely by the checked-in CLI spec, so it can never drift), and
* executing a command — or a *chain* of commands threaded through one
  in-memory ontology — **in-process** via the native extension
  (``owlmake._owlmake.cli``), with no subprocess.

In-process execution notes
--------------------------
The native ``cli`` runs the same dispatch the ``owlmake`` binary uses, on a
large-stack worker thread, with the GIL released. Because it writes to the
process's own stdout/stderr and reads the process's cwd/environment, the
helpers here serialise calls under a lock and, when asked, capture output by
temporarily redirecting file descriptors and apply ``cwd``/``env`` by
save-and-restore. ``timeout`` is not supported in-process (there is no separate
process to kill); ``binary`` is accepted for signature compatibility but
ignored — there is no separate binary to launch.
"""

from __future__ import annotations

import contextlib
import json
import os
import sys
import tempfile
import threading
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Union

from . import _owlmake

__all__ = [
    "StrOrPath",
    "OwlmakeError",
    "OwlmakeResult",
    "version",
    "load_spec",
    "Chain",
]

#: Anything that names a file: a ``str`` or a :class:`os.PathLike` (e.g. a
#: :class:`pathlib.Path`). Path-like values are accepted everywhere a path or
#: identifier is expected.
StrOrPath = Union[str, "os.PathLike[str]"]

# Execution-control keyword names that every generated wrapper accepts. The
# generator suffixes any colliding flag parameter with ``_`` so these stay free.
RUN_KEYS = ("binary", "cwd", "env", "capture", "raise_on_error", "timeout")

# In-process execution touches process-global state (the stdout/stderr file
# descriptors, the cwd, the environment), so calls are serialised.
_EXEC_LOCK = threading.RLock()


class OwlmakeError(RuntimeError):
    """Raised when an owlmake invocation exits non-zero (and error-raising is on).

    The originating :class:`OwlmakeResult` is attached as :attr:`result` so the
    caller can inspect ``returncode``/``stdout``/``stderr`` after catching it.
    """

    def __init__(self, result: "OwlmakeResult") -> None:
        self.result = result
        argv = " ".join(result.argv)
        tail = (result.stderr or result.stdout or "").strip()
        msg = f"owlmake exited with code {result.returncode}: {argv}"
        if tail:
            msg += f"\n{tail}"
        super().__init__(msg)


class OwlmakeResult:
    """Outcome of an owlmake invocation.

    Mirrors the useful surface of :class:`subprocess.CompletedProcess` while
    being explicit about what was run. ``stdout``/``stderr`` are populated only
    when ``capture=True`` (the default).
    """

    __slots__ = ("argv", "returncode", "stdout", "stderr")

    def __init__(
        self,
        argv: Sequence[str],
        returncode: int,
        stdout: Optional[str],
        stderr: Optional[str],
    ) -> None:
        self.argv: List[str] = list(argv)
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

    @property
    def ok(self) -> bool:
        """``True`` iff the command exited zero."""
        return self.returncode == 0

    def check(self) -> "OwlmakeResult":
        """Raise :class:`OwlmakeError` if the command exited non-zero; else self."""
        if self.returncode != 0:
            raise OwlmakeError(self)
        return self

    def __repr__(self) -> str:  # pragma: no cover - cosmetic
        return (
            f"OwlmakeResult(returncode={self.returncode}, "
            f"argv={self.argv!r}, stdout={_clip(self.stdout)!r}, "
            f"stderr={_clip(self.stderr)!r})"
        )


def _clip(s: Optional[str], n: int = 80) -> Optional[str]:
    if s is None:
        return None
    s = s.strip()
    return s if len(s) <= n else s[: n - 1] + "…"


# --------------------------------------------------------------------------- #
# Spec loading
# --------------------------------------------------------------------------- #
_PKG_DIR = Path(__file__).resolve().parent
_SPEC: Optional[Dict[str, Any]] = None
_SPEC_CMDS: Dict[str, Dict[str, Any]] = {}
_SPEC_SSSOM: Dict[str, Dict[str, Any]] = {}


def load_spec() -> Dict[str, Any]:
    """Load (once) and return the bundled CLI spec as a dict."""
    global _SPEC
    if _SPEC is None:
        with open(_PKG_DIR / "_spec.json", "r", encoding="utf-8") as fh:
            _SPEC = json.load(fh)
        for c in _SPEC.get("commands", []):
            _SPEC_CMDS[c["name"]] = c
        for s in _SPEC.get("sssom", {}).get("subcommands", []):
            _SPEC_SSSOM[s["name"]] = s
    return _SPEC


def version() -> str:
    """Return the owlmake version string from the bundled spec."""
    return str(load_spec().get("version", "unknown"))


# --------------------------------------------------------------------------- #
# argv rendering — main commands
# --------------------------------------------------------------------------- #
def _fspath(v: Any) -> str:
    if isinstance(v, os.PathLike):
        return os.fspath(v)
    return str(v)


def _as_list(v: Any) -> List[Any]:
    if isinstance(v, (str, bytes)) or isinstance(v, os.PathLike):
        return [v]
    if isinstance(v, Mapping):
        return [v]
    if isinstance(v, Sequence):
        return list(v)
    return [v]


def _render_arg(arg: Dict[str, Any], value: Any, out: List[str]) -> None:
    if value is None:
        return
    longs = arg.get("longs") or []
    shorts = arg.get("shorts") or []
    flag = f"--{longs[0]}" if longs else f"-{shorts[0]}"
    action = arg["action"]
    possible = set(arg.get("possible_values") or [])

    if action in ("set_true", "set_false"):
        if value:
            out.append(flag)
        return

    if action == "count":
        out.extend([flag] * int(value))
        return

    # `Option<bool>` value flags surface as a value option whose only legal
    # values are true/false; accept a Python bool for ergonomics.
    if possible == {"true", "false"}:
        if isinstance(value, bool):
            out.extend([flag, "true" if value else "false"])
        else:
            out.extend([flag, _fspath(value)])
        return

    variadic = bool(arg.get("variadic"))
    max_values = int(arg.get("max_values", 1))

    if action == "append":
        if variadic:
            out.append(flag)
            out.extend(_fspath(v) for v in _as_list(value))
        elif max_values > 1:
            # Repeatable group flag (e.g. --query-pair FILE OUTPUT): expect an
            # iterable of groups, each of `max_values` items.
            for group in value:
                out.append(flag)
                out.extend(_fspath(v) for v in _as_list(group))
        else:
            for v in _as_list(value):
                out.extend([flag, _fspath(v)])
    else:  # set
        if variadic or max_values > 1:
            out.append(flag)
            out.extend(_fspath(v) for v in _as_list(value))
        else:
            out.extend([flag, _fspath(value)])


def render_command(name: str, values: Mapping[str, Any]) -> List[str]:
    """Render one command segment ``[name, --flag, value, ...]`` from values
    keyed by clap argument id."""
    load_spec()
    spec = _SPEC_CMDS.get(name)
    if spec is None:
        raise KeyError(f"unknown owlmake command: {name!r}")
    out: List[str] = [name]
    for arg in spec["args"]:
        if arg["id"] in values:
            _render_arg(arg, values[arg["id"]], out)
    return out


# --------------------------------------------------------------------------- #
# argv rendering — sssom sub-CLI
# --------------------------------------------------------------------------- #
def render_sssom(name: str, values: Mapping[str, Any], slots: Mapping[str, Any]) -> List[str]:
    """Render an ``sssom <subcommand> ...`` invocation."""
    load_spec()
    spec = _SPEC_SSSOM.get(name)
    if spec is None:
        raise KeyError(f"unknown sssom subcommand: {name!r}")
    out: List[str] = ["sssom", name]

    # Options.
    for opt in spec.get("options", []):
        v = values.get(opt["name"])
        if v is None:
            continue
        flag = opt["long"]
        if opt.get("multiple"):
            if opt.get("nargs", 1) > 1:
                for group in v:
                    out.append(flag)
                    out.extend(_fspath(x) for x in _as_list(group))
            else:
                for x in _as_list(v):
                    out.extend([flag, _fspath(x)])
        elif opt.get("nargs", 1) > 1:
            out.append(flag)
            out.extend(_fspath(x) for x in _as_list(v))
        else:
            out.extend([flag, _fspath(v)])

    # Boolean flags (single form, or --x/--no-x pairs).
    for fl in spec.get("flags", []):
        v = values.get(fl["name"])
        if v is None:
            continue
        if v:
            out.append(fl["true"])
        elif fl.get("false"):
            out.append(fl["false"])

    # Dynamic mapping-slot options (filter/annotate accept one per schema slot).
    for slot, sv in (slots or {}).items():
        flag = "--" + slot.replace("_", "-")
        for x in _as_list(sv):
            out.extend([flag, _fspath(x)])

    # Positionals (trailing).
    for pos in spec.get("positionals", []):
        v = values.get(pos["name"])
        if v is None:
            continue
        if pos.get("variadic"):
            out.extend(_fspath(x) for x in _as_list(v))
        else:
            out.append(_fspath(v))
    return out


# --------------------------------------------------------------------------- #
# Execution (in-process)
# --------------------------------------------------------------------------- #
@contextlib.contextmanager
def _process_context(cwd: Optional[StrOrPath], env: Optional[Mapping[str, str]]):
    """Apply ``cwd``/``env`` to the process for the duration of one in-process
    call, restoring both afterwards. (Process-global; the caller holds the lock.)"""
    old_cwd = None
    old_env = None
    try:
        if cwd is not None:
            old_cwd = os.getcwd()
            os.chdir(os.fspath(cwd))
        if env is not None:
            old_env = dict(os.environ)
            os.environ.update({k: str(v) for k, v in env.items()})
        yield
    finally:
        if old_env is not None:
            os.environ.clear()
            os.environ.update(old_env)
        if old_cwd is not None:
            os.chdir(old_cwd)


def _run_capturing(argv: List[str]) -> tuple:
    """Run ``_owlmake.cli(argv)`` with fds 1/2 redirected to temp files, so the
    native code's stdout/stderr are captured. Returns ``(stdout, stderr, code)``."""
    sys.stdout.flush()
    sys.stderr.flush()
    with tempfile.TemporaryFile() as outf, tempfile.TemporaryFile() as errf:
        saved_out, saved_err = os.dup(1), os.dup(2)
        try:
            os.dup2(outf.fileno(), 1)
            os.dup2(errf.fileno(), 2)
            code = _owlmake.cli(argv)
        finally:
            os.dup2(saved_out, 1)
            os.dup2(saved_err, 2)
            os.close(saved_out)
            os.close(saved_err)
        outf.seek(0)
        errf.seek(0)
        out = outf.read().decode("utf-8", "replace")
        err = errf.read().decode("utf-8", "replace")
    return out, err, code


def execute(
    segment: Sequence[str],
    *,
    binary: Optional[StrOrPath] = None,  # accepted for compatibility; ignored
    cwd: Optional[StrOrPath] = None,
    env: Optional[Mapping[str, str]] = None,
    capture: bool = True,
    raise_on_error: bool = True,
    timeout: Optional[float] = None,
) -> OwlmakeResult:
    """Run a single rendered segment (one or more chained command tokens)
    in-process via the native extension."""
    if timeout is not None:
        raise ValueError(
            "timeout is not supported for in-process execution (there is no "
            "subprocess to interrupt)"
        )
    argv = [_fspath(t) for t in segment]
    with _EXEC_LOCK, _process_context(cwd, env):
        if capture:
            stdout, stderr, code = _run_capturing(argv)
        else:
            code = _owlmake.cli(argv)
            stdout = stderr = None
    result = OwlmakeResult(["owlmake", *argv], code, stdout, stderr)
    if raise_on_error and result.returncode != 0:
        raise OwlmakeError(result)
    return result


def _split_run_opts(kwargs: Dict[str, Any]) -> Dict[str, Any]:
    return {k: kwargs[k] for k in RUN_KEYS if k in kwargs}


def run_command(name: str, values: Mapping[str, Any], **run_opts: Any) -> OwlmakeResult:
    """Render and execute a single command."""
    return execute(render_command(name, values), **run_opts)


def run_sssom(
    name: str, values: Mapping[str, Any], slots: Mapping[str, Any], **run_opts: Any
) -> OwlmakeResult:
    """Render and execute an sssom subcommand."""
    return execute(render_sssom(name, values, slots), **run_opts)


def run_raw(args: Sequence[StrOrPath], **run_opts: Any) -> OwlmakeResult:
    """Execute a verbatim argv in-process (escape hatch / `jq` etc.)."""
    return execute([_fspath(a) for a in args], **run_opts)


# --------------------------------------------------------------------------- #
# Chain — command chaining through one in-memory ontology
# --------------------------------------------------------------------------- #
class Chain:
    """Builder for a command chain.

    owlmake can thread a single in-memory ontology through several commands in
    one invocation, e.g. ``merge -i a.owl reason reduce -o out.owl``.
    A plain sequence of per-command Python calls would instead reload from disk
    each time; :class:`Chain` preserves the single-pass, in-memory semantics by
    concatenating every command's tokens into one in-process invocation.

    The per-command methods are attached by the generated ``_commands`` module
    (one method per command, same signature as the module-level function but
    returning ``self`` for fluent chaining). Call :meth:`run` to execute.

        owlmake.chain().merge(input="a.owl").reason(reasoner="elk").reduce() \\
            .convert(output="out.owl").run()
    """

    def __init__(self) -> None:
        self._segments: List[List[str]] = []

    def _add(self, name: str, values: Mapping[str, Any]) -> "Chain":
        self._segments.append(render_command(name, values))
        return self

    def argv(self) -> List[str]:
        """The flattened argv (without the program name) this chain will run."""
        return [tok for seg in self._segments for tok in seg]

    def run(self, **run_opts: Any) -> OwlmakeResult:
        """Execute the whole chain in a single in-process invocation."""
        if not self._segments:
            raise ValueError("empty chain: add at least one command before run()")
        return execute(self.argv(), **_split_run_opts(run_opts))

    def __repr__(self) -> str:  # pragma: no cover - cosmetic
        return f"Chain({self.argv()!r})"


def main(argv: Optional[Sequence[str]] = None) -> int:
    """``python -m owlmake ...`` — dispatch argv in-process and return the exit
    code (output streams to this process's stdout/stderr)."""
    args = list(sys.argv[1:] if argv is None else argv)
    return _owlmake.cli(args)
