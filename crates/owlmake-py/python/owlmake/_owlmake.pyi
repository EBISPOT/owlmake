"""Type stubs for the native owlmake extension (owlmake._owlmake)."""

from typing import Any, Dict, List, Optional, Tuple

class Ontology:
    """An in-memory OWL ontology: the full horned-owl object model plus its
    prefix map. The unit every operation reads and writes."""

    def __init__(self) -> None:
        """An empty ontology, ready for :meth:`add_axioms`."""

    @staticmethod
    def parse(bytes: bytes, format: str) -> "Ontology":
        """Parse an ontology from ``bytes`` in the given serialization format
        (``"ofn"``, ``"owl"``, ``"obo"``, ``"ttl"``, …)."""

    def serialize(self, format: str) -> bytes:
        """Serialize the ontology to bytes in the given format."""

    def reason(self, reasoner: str) -> None:
        """Classify and assert the inferred axioms in place. ``"elk"`` /
        ``"owlmake"`` use the built-in EL reasoner; ``"whelk"`` / ``"hermit"`` /
        ``"jfact"`` use the external backends (native build only)."""

    def reduce(self) -> None:
        """Transitive reduction of the class hierarchy, in place."""

    def relax(self) -> None:
        """Relax equivalence/expression axioms to ``SubClassOf`` (ROBOT relax)."""

    def merge(self, other: "Ontology") -> None:
        """Merge ``other`` into this ontology in place (this is the base)."""

    def add_axioms(self, ofn: str) -> int:
        """Add the axioms in an OWL Functional-Syntax fragment, resolved against
        the ontology's prefixes. Returns the number newly inserted."""

    def remove_axioms(self, ofn: str) -> int:
        """Remove the axioms in an OWL Functional-Syntax fragment. Returns the
        number removed."""

    def axiom_count(self) -> int:
        """Number of components (logical axioms + ontology metadata)."""

    def __len__(self) -> int: ...
    def classes(self) -> List[str]:
        """The IRIs of every declared class."""

    def object_properties(self) -> List[str]:
        """The IRIs of every declared object property."""

    def subclass_pairs(self) -> List[Tuple[str, str]]:
        """Every named ``SubClassOf`` relation as ``(sub, super)`` IRI tuples."""

    def __repr__(self) -> str: ...

    def filter(self, terms: List[str], select: Optional[List[str]] = ..., signature: bool = ...) -> None:
        """Keep only the axioms mentioning ``terms`` (ROBOT ``filter``), in place.
        ``select`` chooses related-entity expansion; ``signature`` keeps an axiom
        if any of its signature is selected (vs the whole signature by default)."""

    def remove(self, terms: List[str], select: Optional[List[str]] = ...) -> None:
        """Remove the axioms mentioning ``terms`` (ROBOT ``remove``), in place."""

    def annotate(self, ontology_iri: Optional[str] = ..., version_iri: Optional[str] = ..., annotations: Optional[List[str]] = ...) -> None:
        """Set the ontology/version IRIs and add ontology annotations (ROBOT
        ``annotate``), in place. ``annotations`` is a flat list of alternating
        ``prop, value`` tokens (e.g. ``["rdfs:comment", "hello"]``)."""

    def rename(self, mapping: Dict[str, str]) -> None:
        """Bulk-rename entity IRIs from an old→new dict (ROBOT ``rename``), in
        place."""

    def materialize(self, properties: Optional[List[str]] = ...) -> None:
        """Assert inferred existential restrictions (ROBOT ``materialize``), in
        place. ``properties`` limits which object properties to materialize (all
        if empty)."""

    def extract(self, terms: List[str], method: str = ...) -> "Ontology":
        """Extract a module for a seed term set (ROBOT ``extract``) as a new
        ontology, leaving this one unchanged. ``method`` is
        ``BOT``/``TOP``/``STAR``/``MIREOT``."""

    def diff(self, other: "Ontology") -> str:
        """A human-readable diff against another ontology (ROBOT ``diff``)."""

    def measure(self) -> str:
        """Ontology metrics (ROBOT ``measure``) as tab-separated ``metric\\tvalue``
        rows."""
    def query(self, sparql: str) -> str: ...
    def query_records(self, sparql: str, reasoner: Optional[str] = ...) -> List[Dict[str, str]]:
        """SPARQL SELECT rows as ``{column: value}`` dicts (for DataFrames). With
        ``reasoner`` set, runs over the reasoned/entailed graph."""

    def query_dataframe(self, sparql: str, reasoner: Optional[str] = ..., backend: str = ...) -> Any:
        """SPARQL SELECT result as a pandas (default) or polars DataFrame. With
        ``reasoner`` set, runs over the reasoned/entailed graph."""

    def dl_query(self, expression: str, kind: str = ..., reasoner: str = ...) -> List[str]:
        """Protégé-style DL query: a Manchester-syntax class ``expression``
        answered by the reasoner. ``kind`` is ``subclasses``/``descendants``/
        ``superclasses``/``ancestors``/``equivalent``/``instances``."""

    def dl_query_dataframe(self, expression: str, kind: str = ..., reasoner: str = ..., backend: str = ...) -> Any:
        """A DL query as a one-column (``entity``) pandas/polars DataFrame."""

    def template(self, table: Any) -> None:
        """Apply a ROBOT template table (TSV text, list of row dicts, or a
        pandas/polars DataFrame; first data row holds the template strings)."""


class MappingSet:
    """An in-memory SSSOM mapping set; rows are ``{slot: value}`` dicts."""

    def __init__(self) -> None: ...
    @staticmethod
    def parse(text: str, format: str = ...) -> "MappingSet":
        """Parse a mapping set from ``text`` (``"tsv"``/``"csv"``/``"json"``)."""
    def serialize(self, format: str = ...) -> str:
        """Serialize the mapping set to text in the given format."""
    def records(self) -> List[Dict[str, str]]:
        """The mapping rows as ``{slot: value}`` dicts (for DataFrames)."""
    @staticmethod
    def from_records(records: List[Dict[str, str]], curie_map: Optional[Dict[str, str]] = ...) -> "MappingSet":
        """Build a mapping set from ``{slot: value}`` row dicts and an optional
        CURIE map."""
    @property
    def curie_map(self) -> Dict[str, str]:
        """The CURIE prefix map (prefix → namespace IRI)."""
    @curie_map.setter
    def curie_map(self, value: Dict[str, str]) -> None: ...
    def sort(self) -> None:
        """Sort columns into canonical order and rows by ``(subject, predicate,
        object)``, in place."""
    def canonicalize(self) -> None:
        """Canonicalize the mapping set (ROBOT/sssom canonical form), in place."""
    def condense(self) -> None:
        """Condense multi-valued slots into single rows, in place."""
    def propagate(self) -> None:
        """Propagate set-level slots onto each mapping row, in place."""
    def merge(self, other: "MappingSet") -> None:
        """Merge another mapping set into this one (rows appended, CURIE maps
        unioned), in place."""
    def __len__(self) -> int: ...
    def __repr__(self) -> str: ...


def cli(args: List[str]) -> int: ...
def cli_spec() -> str: ...
def sssom_convert(input: str, to: str, from_format: str = ...) -> str: ...
def dosdp(pattern_yaml: str, data: Any) -> Ontology: ...
