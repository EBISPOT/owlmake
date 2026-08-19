//! Assemble the named-graph dataset and serialize it as N-Quads.
//!
//! There are four named graphs in an in-memory `oxigraph` store. The merged
//! ontology, build metadata, opposites, prefixes, `is_defined_by` and
//! information content all go into the ontology graph; the redundant and
//! non-redundant property graphs get one each; the biolink model and the
//! categories derived from it share a fourth. Categories come from a SELECT
//! restricted to the ontology and biolink graphs and are asserted one triple at
//! a time, so a row that does not bind two IRIs is skipped rather than aborting
//! the assembly. The store is then dumped as N-Quads, which keep the graph
//! names alongside the triples.

use anyhow::{anyhow, Result};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{GraphName, NamedNode, NamedNodeRef, QuadRef, Term};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

use crate::io::{self, Format};
use crate::model::Model;
use crate::ubergraph::IriTriple;

/// A growing named-graph dataset backed by oxigraph.
pub struct Dataset {
    store: Store,
}

impl Dataset {
    pub fn new() -> Result<Dataset> {
        Ok(Dataset {
            store: Store::new().map_err(|e| anyhow!("store init: {e}"))?,
        })
    }

    /// Load an RDF/XML or Turtle byte blob into the named graph `graph`.
    pub fn load_bytes(&self, bytes: &[u8], format: RdfFormat, graph: &str) -> Result<()> {
        let g = NamedNodeRef::new(graph).map_err(|e| anyhow!("bad graph IRI {graph}: {e}"))?;
        self.store
            .load_from_slice(
                RdfParser::from_format(format)
                    .without_named_graphs()
                    .with_default_graph(g),
                bytes,
            )
            .map_err(|e| anyhow!("loading into <{graph}>: {e}"))?;
        Ok(())
    }

    /// Load N-Triples lines into the named graph `graph`.
    pub fn load_ntriples(&self, lines: &str, graph: &str) -> Result<()> {
        self.load_bytes(lines.as_bytes(), RdfFormat::NTriples, graph)
    }

    /// Load a model (serialized to RDF/XML) into the named graph `graph`.
    pub fn load_model(&self, model: &Model, graph: &str) -> Result<()> {
        let mut rdf = Vec::new();
        io::write_to_ref(model, &mut rdf, Format::RdfXml)?;
        self.load_bytes(&rdf, RdfFormat::RdfXml, graph)
    }

    /// Load IRI triples into the named graph `graph`.
    pub fn load_iri_triples(&self, triples: &[IriTriple], graph: &str) -> Result<()> {
        let nt = crate::ubergraph::iri_triples_to_ntriples(triples);
        self.load_ntriples(&nt, graph)
    }

    /// Run a SPARQL 1.1 update over the dataset.
    /// SPARQL defines no built-in prefixes, so the standard ones (`rdf:`,
    /// `rdfs:`, `owl:`, `xsd:`) are prepended and an update that uses them
    /// without declaring them still parses.
    pub fn update(&self, sparql: &str) -> Result<()> {
        let prefixed = with_standard_prefixes(sparql);
        self.store
            .update(prefixed.as_str())
            .map_err(|e| anyhow!("SPARQL update: {e}"))?;
        Ok(())
    }

    /// Run a SELECT over the **union of all named graphs** as the default graph:
    /// a pattern outside any `GRAPH` clause matches quads in every graph, so it
    /// can join facts that were loaded separately. Returns the projected column
    /// names and one row of bare term strings (IRIs without `<>`, literals as
    /// their lexical value) per solution.
    pub fn select_union(&self, sparql: &str) -> Result<(Vec<String>, Vec<Vec<String>>)> {
        self.select_with(sparql, None)
    }

    /// Like [`select_union`](Self::select_union) but with the default graph
    /// restricted to the named graphs in `graphs`. Used to keep the biolink/KGX
    /// property-path queries off the huge redundant closure (which is not needed
    /// for `subClassOf*` reachability and makes oxigraph's path eval explode).
    pub fn select_over(
        &self,
        graphs: &[&str],
        sparql: &str,
    ) -> Result<(Vec<String>, Vec<Vec<String>>)> {
        self.select_with(sparql, Some(graphs))
    }

    fn select_with(
        &self,
        sparql: &str,
        graphs: Option<&[&str]>,
    ) -> Result<(Vec<String>, Vec<Vec<String>>)> {
        let prefixed = with_standard_prefixes(sparql);
        let mut prepared = SparqlEvaluator::new()
            .parse_query(&prefixed)
            .map_err(|e| anyhow!("SPARQL parse error: {e}"))?;
        match graphs {
            None => prepared.dataset_mut().set_default_graph_as_union(),
            Some(gs) => {
                let names: Vec<GraphName> = gs
                    .iter()
                    .filter_map(|g| NamedNode::new(*g).ok().map(GraphName::NamedNode))
                    .collect();
                prepared.dataset_mut().set_default_graph(names);
            }
        }
        let results = prepared
            .on_store(&self.store)
            .execute()
            .map_err(|e| anyhow!("SPARQL error: {e}"))?;
        match results {
            QueryResults::Solutions(solutions) => {
                let columns: Vec<String> =
                    solutions.variables().iter().map(|v| v.as_str().to_string()).collect();
                let mut rows = Vec::new();
                for sol in solutions {
                    let sol = sol.map_err(|e| anyhow!("solution error: {e}"))?;
                    rows.push(
                        columns
                            .iter()
                            .map(|c| sol.get(c.as_str()).map(term_to_string).unwrap_or_default())
                            .collect(),
                    );
                }
                Ok((columns, rows))
            }
            _ => Err(anyhow!("expected a SELECT query")),
        }
    }

    /// Insert a single `s p o` triple (all IRIs) into the named graph `graph`.
    pub fn insert_triple(&self, s: &str, p: &str, o: &str, graph: &str) -> Result<()> {
        let sn = NamedNodeRef::new(s).map_err(|e| anyhow!("bad IRI {s}: {e}"))?;
        let pn = NamedNodeRef::new(p).map_err(|e| anyhow!("bad IRI {p}: {e}"))?;
        let on = NamedNodeRef::new(o).map_err(|e| anyhow!("bad IRI {o}: {e}"))?;
        let gn = NamedNodeRef::new(graph).map_err(|e| anyhow!("bad graph {graph}: {e}"))?;
        self.store
            .insert(QuadRef::new(sn, pn, on, gn))
            .map_err(|e| anyhow!("insert: {e}"))?;
        Ok(())
    }

    /// Serialize the whole dataset as N-Quads, with lines sorted for
    /// determinism (RDF is set-based, so order is immaterial to graph identity).
    pub fn to_nquads_sorted(&self) -> Result<String> {
        let buf = self
            .store
            .dump_to_writer(RdfFormat::NQuads, Vec::new())
            .map_err(|e| anyhow!("dumping N-Quads: {e}"))?;
        let text = String::from_utf8(buf).map_err(|e| anyhow!("utf8: {e}"))?;
        let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        lines.sort_unstable();
        lines.dedup();
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        Ok(out)
    }

    /// Number of quads currently in the store.
    pub fn len(&self) -> usize {
        self.store.len().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Prepend the standard prefix declarations a SPARQL string may assume but not
/// declare (notably `rdf:`), without duplicating ones already present.
fn with_standard_prefixes(sparql: &str) -> String {
    let defaults = [
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("owl", "http://www.w3.org/2002/07/owl#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ];
    let lower = sparql.to_ascii_lowercase();
    let mut prefix = String::new();
    for (name, ns) in defaults {
        if !lower.contains(&format!("prefix {name}:")) {
            prefix.push_str(&format!("PREFIX {name}: <{ns}>\n"));
        }
    }
    prefix.push_str(sparql);
    prefix
}

/// Bare string form of an RDF term: IRIs without `<>`, literals as their
/// lexical value, blank nodes as `_:id`. Suited to KGX/CSV consumers.
fn term_to_string(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => n.as_str().to_string(),
        Term::BlankNode(b) => format!("_:{}", b.as_str()),
        Term::Literal(l) => l.value().to_string(),
        #[allow(unreachable_patterns)]
        other => other.to_string(),
    }
}

/// Render SELECT rows as a TSV with the given header (the KGX column names).
pub fn rows_to_tsv(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = header.join("\t");
    out.push('\n');
    let mut lines: Vec<String> = rows.iter().map(|r| r.join("\t")).collect();
    lines.sort_unstable();
    lines.dedup();
    out.push_str(&lines.join("\n"));
    if !lines.is_empty() {
        out.push('\n');
    }
    out
}

/// KGX-style edge table: `subject<TAB>predicate<TAB>object`, header included,
/// from a property/subclass graph. Rows are sorted and deduplicated, so the
/// table is byte-stable across builds of the same graph.
pub fn edge_table(triples: &[IriTriple]) -> String {
    let mut out = String::from("subject\tpredicate\tobject\n");
    let mut rows: Vec<String> = triples
        .iter()
        .map(|(s, p, o)| format!("{s}\t{p}\t{o}"))
        .collect();
    rows.sort_unstable();
    rows.dedup();
    out.push_str(&rows.join("\n"));
    if !rows.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ubergraph::{graph, iri};

    #[test]
    fn named_graph_roundtrip_to_nquads() {
        let ds = Dataset::new().unwrap();
        let red = vec![(
            "http://purl.obolibrary.org/obo/CL_0000000".to_string(),
            "http://purl.obolibrary.org/obo/BFO_0000050".to_string(),
            "http://purl.obolibrary.org/obo/UBERON_0000000".to_string(),
        )];
        ds.load_iri_triples(&red, graph::REDUNDANT).unwrap();
        let nq = ds.to_nquads_sorted().unwrap();
        assert!(nq.contains(&format!("<{}>", graph::REDUNDANT)));
        assert!(nq.contains("CL_0000000"));
        assert_eq!(ds.len(), 1);
    }

    #[test]
    fn ic_decimal_literal_loads_and_dumps() {
        let ds = Dataset::new().unwrap();
        let line = format!(
            "<http://purl.obolibrary.org/obo/CL_0000000> <{}> \"100\"^^<{}> .",
            iri::NORMALIZED_IC,
            iri::XSD_DECIMAL
        );
        ds.load_ntriples(&line, graph::ONTOLOGY).unwrap();
        let nq = ds.to_nquads_sorted().unwrap();
        assert!(nq.contains(iri::NORMALIZED_IC));
        assert!(nq.contains(&format!("<{}>", graph::ONTOLOGY)));
    }

    #[test]
    fn select_union_joins_across_named_graphs() {
        // A subClassOf edge in the ontology graph + a mapping in the biolink
        // graph; a union SELECT must see both and join them.
        let ds = Dataset::new().unwrap();
        ds.load_ntriples(
            "<http://ex/A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex/B> .",
            graph::ONTOLOGY,
        )
        .unwrap();
        ds.load_ntriples(
            "<http://ex/B> <http://ex/maps> <http://ex/CatX> .",
            graph::BIOLINK,
        )
        .unwrap();
        let (cols, rows) = ds
            .select_union(
                "SELECT ?a ?cat WHERE { ?a <http://www.w3.org/2000/01/rdf-schema#subClassOf> ?b . ?b <http://ex/maps> ?cat }",
            )
            .unwrap();
        assert_eq!(cols, vec!["a".to_string(), "cat".to_string()]);
        assert_eq!(rows, vec![vec!["http://ex/A".to_string(), "http://ex/CatX".to_string()]]);

        // insert_triple lands in the requested graph and shows up in the dump.
        ds.insert_triple("http://ex/A", "http://ex/category", "http://ex/CatX", graph::BIOLINK)
            .unwrap();
        let nq = ds.to_nquads_sorted().unwrap();
        assert!(nq.contains("<http://ex/category>") && nq.contains(&format!("<{}>", graph::BIOLINK)));
    }

    #[test]
    fn property_path_with_not_exists_is_correct() {
        // A `p+` path in the outer pattern and again inside a FILTER NOT EXISTS,
        // over a chain A→B→C→D — the shape the KGX/biolink queries use. Guards
        // our `select_over` usage against returning wrong rows.
        let ds = Dataset::new().unwrap();
        let p = "http://ex/p";
        let nt = format!(
            "<http://ex/A> <{p}> <http://ex/B> .\n<http://ex/B> <{p}> <http://ex/C> .\n<http://ex/C> <{p}> <http://ex/D> .\n"
        );
        ds.load_ntriples(&nt, graph::ONTOLOGY).unwrap();
        // Keep ?x with x p+ D but with NO intermediate y (x p+ y, y p+ D) — i.e.
        // only the most-specific predecessor C.
        let q = format!(
            "SELECT ?x WHERE {{ ?x <{p}>+ <http://ex/D> FILTER NOT EXISTS {{ ?x <{p}>+ ?y . ?y <{p}>+ <http://ex/D> }} }}"
        );
        let (_, rows) = ds.select_over(&[graph::ONTOLOGY], &q).unwrap();
        let got: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(got, vec!["http://ex/C"], "only C has no closer intermediate to D");
    }

    #[test]
    fn edge_table_has_header_and_sorted_rows() {
        let triples = vec![
            ("b".into(), "p".into(), "o".into()),
            ("a".into(), "p".into(), "o".into()),
        ];
        let table = edge_table(&triples);
        assert_eq!(table, "subject\tpredicate\tobject\na\tp\to\nb\tp\to\n");
    }
}
