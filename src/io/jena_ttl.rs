//! The pretty Turtle layout a CONSTRUCT's graph is written in.
//!
//! Two commands write constructed graphs as Turtle and each has its own layout,
//! kept here as a [`Dialect`]:
//!
//! * [`Dialect::Query`] — `om query --format ttl` (MONDO's `mirror-hgnc`):
//!   prefixes in the writer's sixteen-bucket map order, aligned to column 15,
//!   ` ;` between predicates;
//! * [`Dialect::Arq`] — `om arq` writing its default CONSTRUCT output (MONDO's
//!   `mirror-ncbigene`): prefixes sorted and aligned past the longest name, `;`
//!   flush against the object.
//!
//! Both cluster a subject's triples with the predicates column-aligned,
//! `rdf:type` first as `a`, RDF/RDFS predicates next, the rest in IRI order —
//! and read the subjects back in the graph's own slot order (see
//! [`crate::sparql::jena_order`]).

use crate::sparql::jena_order as jo;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS_NS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const LONG_SUBJECT: usize = 20;
const LONG_PREDICATE: usize = 30;
const INDENT_PREDICATE: usize = 8;
const INDENT_OBJECT: usize = 8;
const MIN_PREDICATE: usize = 6;
const GAP_P_O: usize = 2;
const PREFIX_IRI: usize = 15;

/// Which command's layout to write. See the module doc.
#[derive(Clone, Copy, PartialEq)]
pub enum Dialect {
    Query,
    Arq,
}

/// A node in a constructed graph: an IRI, a blank node by label, or a literal
/// reduced to lexical form, language and datatype (`dt: None` for a plain
/// string — an explicit `xsd:string` is the same literal and is folded to it on
/// construction).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum JNode {
    Iri(String),
    Bnode(String),
    Lit { lex: String, lang: Option<String>, dt: Option<String> },
}

/// A constructed graph replayed the way the writer lays it out: subjects in the
/// order they gained their first triple, each subject's triples in insertion
/// order, duplicates dropped on arrival.
#[derive(Default)]
pub struct JenaGraph {
    subjects: Vec<JNode>,
    clusters: std::collections::HashMap<JNode, Vec<(String, JNode)>>,
    seen: std::collections::HashSet<(JNode, String, JNode)>,
}

impl JenaGraph {
    pub fn add(&mut self, s: JNode, p: &str, o: JNode) {
        if !self.seen.insert((s.clone(), p.to_string(), o.clone())) {
            return;
        }
        match self.clusters.entry(s.clone()) {
            std::collections::hash_map::Entry::Vacant(e) => {
                self.subjects.push(s);
                e.insert(vec![(p.to_string(), o)]);
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.get_mut().push((p.to_string(), o));
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.subjects.is_empty()
    }
}

/// The prefix declarations in the order the writer's own prefix map yields
/// them: a sixteen-bucket table that doubles past three quarters full, each key
/// in the bucket its spread hash selects, buckets read in ascending order and
/// each bucket in insertion order.
fn prefix_map_order(pairs: &[(String, String)]) -> Vec<(String, String)> {
    use jo::java_string_hash;
    // Re-declaring a prefix keeps its position and takes the last namespace.
    let mut keys: Vec<String> = Vec::new();
    let mut vals: std::collections::HashMap<String, String> = Default::default();
    for (k, v) in pairs {
        if vals.insert(k.clone(), v.clone()).is_none() {
            keys.push(k.clone());
        }
    }
    let mut cap = 16usize;
    while keys.len() > cap * 3 / 4 {
        cap *= 2;
    }
    let mut buckets: Vec<Vec<&String>> = vec![Vec::new(); cap];
    for k in &keys {
        let h = java_string_hash(k);
        let spread = h ^ ((h as u32) >> 16) as i32;
        buckets[(spread as usize) & (cap - 1)].push(k);
    }
    buckets
        .into_iter()
        .flatten()
        .map(|k| (k.clone(), vals[k].clone()))
        .collect()
}

/// Character classes of a prefixed name.
fn is_pn_chars_base(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z'
        | '\u{00C0}'..='\u{00D6}' | '\u{00D8}'..='\u{00F6}' | '\u{00F8}'..='\u{02FF}'
        | '\u{0370}'..='\u{037D}' | '\u{037F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}
fn is_pn_chars_u(c: char) -> bool {
    is_pn_chars_base(c) || c == '_'
}
fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{00B7}'
        || ('\u{0300}'..='\u{036F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
}

/// Whether a local name can follow `prefix:` in this layout.
fn safe_local(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return true;
    }
    let first = chars[0];
    if !(is_pn_chars_u(first) || first.is_ascii_digit() || first == ':') {
        return false;
    }
    let last = chars[chars.len() - 1];
    if chars.len() > 1 && !(is_pn_chars(last) || last == ':') {
        return false;
    }
    chars[1..chars.len().saturating_sub(1)]
        .iter()
        .all(|&c| is_pn_chars(c) || c == '.' || c == ':')
}

/// Whether a prefix name is usable in this layout.
fn safe_prefix(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return true;
    }
    if !is_pn_chars_base(chars[0]) {
        return false;
    }
    let last = chars[chars.len() - 1];
    if chars.len() > 1 && !is_pn_chars(last) {
        return false;
    }
    chars[1..chars.len().saturating_sub(1)].iter().all(|&c| is_pn_chars(c) || c == '.')
}

/// An IRI as this layout prints it: `prefix:local` when the namespace up to the
/// last `#` or `/` is declared and both halves are legal, `<iri>` otherwise.
fn pretty_iri(uri: &str, prefixes: &[(String, String)]) -> String {
    // Any declared namespace that is a string prefix of the IRI shortens it —
    // the namespace does not need to end at `#` or `/`, so `MONDO_1011580`
    // under `MONDO: …/MONDO_` prints as `MONDO:1011580`. The longest declared
    // namespace wins.
    let mut best: Option<(&String, usize)> = None;
    for (p, n) in prefixes {
        if let Some(local) = uri.strip_prefix(n.as_str()) {
            if !n.is_empty()
                && safe_prefix(p)
                && safe_local(local)
                && best.map_or(true, |(_, len)| n.len() > len)
            {
                best = Some((p, n.len()));
            }
        }
    }
    if let Some((p, len)) = best {
        return format!("{p}:{}", &uri[len..]);
    }
    format!("<{uri}>")
}

/// A literal as this layout prints it.
fn pretty_literal(
    lex: &str,
    lang: &Option<String>,
    dt: &Option<String>,
    prefixes: &[(String, String)],
) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                '\u{000C}' => out.push_str("\\f"),
                _ => out.push(c),
            }
        }
        out
    }
    if let Some(l) = lang {
        return format!("\"{}\"@{l}", esc(lex));
    }
    let Some(dt) = dt else { return format!("\"{}\"", esc(lex)) };
    let bare = match dt.as_str() {
        "http://www.w3.org/2001/XMLSchema#integer" => valid_integer(lex),
        "http://www.w3.org/2001/XMLSchema#decimal" => valid_decimal(lex),
        "http://www.w3.org/2001/XMLSchema#double" => valid_double(lex),
        "http://www.w3.org/2001/XMLSchema#boolean" => lex == "true" || lex == "false",
        _ => false,
    };
    if bare {
        return lex.to_string();
    }
    format!("\"{}\"^^{}", esc(lex), pretty_iri(dt, prefixes))
}

fn valid_integer(lex: &str) -> bool {
    let d = lex.strip_prefix(['+', '-']).unwrap_or(lex);
    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
}
fn valid_decimal(lex: &str) -> bool {
    let d = lex.strip_prefix(['+', '-']).unwrap_or(lex);
    let Some((a, b)) = d.split_once('.') else { return false };
    a.bytes().all(|c| c.is_ascii_digit())
        && !b.is_empty()
        && b.bytes().all(|c| c.is_ascii_digit())
}
fn valid_double(lex: &str) -> bool {
    let d = lex.strip_prefix(['+', '-']).unwrap_or(lex);
    let (mantissa, exp) = match d.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (d, None),
    };
    let Some(e) = exp else { return false };
    let e = e.strip_prefix(['+', '-']).unwrap_or(e);
    if e.is_empty() || !e.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let (a, b) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    (!a.is_empty() || !b.is_empty())
        && a.bytes().all(|c| c.is_ascii_digit())
        && b.bytes().all(|c| c.is_ascii_digit())
}

/// A predicate's sort class: `rdf:type` first, the RDF and RDFS namespaces
/// next, everything else last; ties break on the IRI's UTF-16 code units.
fn predicate_order(a: &str, b: &str) -> std::cmp::Ordering {
    let class = |p: &str| -> u8 {
        if p == RDF_TYPE {
            0
        } else if p.starts_with(RDF_NS) || p.starts_with(RDFS_NS) {
            1
        } else {
            2
        }
    };
    class(a).cmp(&class(b)).then_with(|| a.encode_utf16().cmp(b.encode_utf16()))
}

/// The writer's running line state: everything appends through it so the
/// column arithmetic (alignment, indents) lives in one place.
struct Out {
    buf: String,
    col: usize,
    /// True right after a newline; the next print pads to `indent` first.
    at_line_start: bool,
    indent: usize,
}

impl Out {
    fn new() -> Out {
        Out { buf: String::new(), col: 0, at_line_start: false, indent: 0 }
    }
    fn print(&mut self, s: &str) {
        if self.at_line_start {
            self.at_line_start = false;
            while self.col < self.indent {
                self.buf.push(' ');
                self.col += 1;
            }
        }
        self.buf.push_str(s);
        self.col += s.chars().count();
    }
    fn println(&mut self) {
        self.buf.push('\n');
        self.col = 0;
        self.at_line_start = true;
    }
    /// Pad with spaces to column `indent + col` (no-op when already past it).
    fn pad_to(&mut self, col: usize) {
        if self.at_line_start {
            self.at_line_start = false;
            while self.col < self.indent {
                self.buf.push(' ');
                self.col += 1;
            }
        }
        while self.col < self.indent + col {
            self.buf.push(' ');
            self.col += 1;
        }
    }
    fn gap(&mut self, n: usize) {
        for _ in 0..n {
            self.print(" ");
        }
    }
}

/// Serialize a replayed graph. `None` when the graph uses a shape this layout
/// does not cover — a blank node referenced more than once, a blank-node cycle,
/// an RDF list — and the caller keeps its plain serializer.
pub fn write_pretty(
    graph: &JenaGraph,
    prefixes: &[(String, String)],
    dialect: Dialect,
) -> Option<Vec<u8>> {
    // Blank-node shape: each blank node is written inline, inside the one
    // triple that references it. Anything else is outside this layout.
    let mut incoming: std::collections::HashMap<&str, usize> = Default::default();
    for objs in graph.clusters.values() {
        for (_, o) in objs {
            if let JNode::Bnode(b) = o {
                *incoming.entry(b.as_str()).or_insert(0) += 1;
            }
        }
    }
    for s in &graph.subjects {
        if let JNode::Bnode(b) = s {
            if incoming.get(b.as_str()).copied().unwrap_or(0) != 1 {
                return None;
            }
        }
    }
    for objs in graph.clusters.values() {
        for (p, o) in objs {
            if p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#first"
                || p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest"
            {
                return None;
            }
            if let JNode::Bnode(b) = o {
                // A referenced blank node with no cluster of its own would print
                // as `[] `; none of the graphs this writes have one.
                if !graph.clusters.contains_key(&JNode::Bnode(b.clone())) {
                    return None;
                }
            }
        }
    }

    let prefixes: Vec<(String, String)> = match dialect {
        Dialect::Query => prefix_map_order(prefixes),
        Dialect::Arq => {
            let mut v = prefix_map_order(prefixes);
            v.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            v
        }
    };

    let mut out = Out::new();
    let prefix_col = match dialect {
        Dialect::Query => PREFIX_IRI,
        Dialect::Arq => {
            10 + prefixes.iter().map(|(p, _)| p.chars().count()).max().unwrap_or(0)
        }
    };
    for (p, iri) in &prefixes {
        out.print(&format!("@prefix {p}: "));
        out.pad_to(prefix_col);
        out.print(&format!("<{iri}> ."));
        out.println();
    }
    if !prefixes.is_empty() && !graph.is_empty() {
        out.println();
        out.at_line_start = false;
    }

    let node_label = |n: &JNode| -> String {
        match n {
            JNode::Iri(u) => u.clone(),
            JNode::Bnode(b) => b.clone(),
            JNode::Lit { .. } => unreachable!("a literal is never a subject"),
        }
    };
    let hashes: Vec<i32> = graph.subjects.iter().map(|s| jo::node_hash(&node_label(s))).collect();
    // The query layout reads the subject slots ascending; the arq layout reads
    // them descending.
    let mut order = jo::bunch_map_order(&hashes);
    if dialect == Dialect::Arq {
        order.reverse();
    }

    // Whether `rdf:type` prints as `a`. The query layout always writes `a`;
    // the arq layout writes it only while no declared prefix names the RDF
    // namespace.
    let rdf_declared = match dialect {
        Dialect::Query => false,
        Dialect::Arq => prefixes.iter().any(|(_, ns)| ns == RDF_NS),
    };
    let sep = match dialect {
        Dialect::Query => " ;",
        Dialect::Arq => ";",
    };

    let node_str = |n: &JNode| -> String {
        match n {
            JNode::Iri(u) => pretty_iri(u, &prefixes),
            JNode::Bnode(b) => format!("_:{b}"),
            JNode::Lit { lex, lang, dt } => pretty_literal(lex, lang, dt, &prefixes),
        }
    };
    let pred_str = |p: &str| -> String {
        if p == RDF_TYPE && !rdf_declared {
            "a".to_string()
        } else {
            pretty_iri(p, &prefixes)
        }
    };

    /// One subject's triples in the order its bunch reads back: reversed while
    /// small; slot order once hashed — ascending in the query layout,
    /// descending in the arq layout.
    fn bunch_iter<'g>(
        subject: &JNode,
        cluster: &'g [(String, JNode)],
        dialect: Dialect,
    ) -> Vec<&'g (String, JNode)> {
        if cluster.len() < 10 {
            return cluster.iter().rev().collect();
        }
        let sh = jo::node_hash(match subject {
            JNode::Iri(u) => u,
            JNode::Bnode(b) => b,
            JNode::Lit { .. } => unreachable!(),
        });
        let th: Vec<Option<i32>> = cluster
            .iter()
            .map(|(p, o)| {
                let oh = match o {
                    JNode::Iri(u) => Some(jo::node_hash(u)),
                    JNode::Bnode(b) => Some(jo::node_hash(b)),
                    JNode::Lit { lex, lang, dt } => crate::sparql::literal_value_hash(
                        lex,
                        dt.as_deref().unwrap_or("http://www.w3.org/2001/XMLSchema#string"),
                        lang.is_some(),
                    ),
                };
                oh.map(|oh| jo::triple_hash(sh, jo::node_hash(p), oh))
            })
            .collect();
        let mut order = jo::bunch_order(&th);
        if dialect == Dialect::Arq {
            order.reverse();
        }
        order.into_iter().map(|i| &cluster[i]).collect()
    }

    /// The cluster grouped by predicate: predicates sorted, each group's
    /// objects in bunch order.
    fn group<'g>(
        subject: &JNode,
        cluster: &'g [(String, JNode)],
        dialect: Dialect,
    ) -> Vec<(&'g str, Vec<&'g JNode>)> {
        let mut preds: Vec<&str> = Vec::new();
        let mut by: std::collections::HashMap<&str, Vec<&JNode>> = Default::default();
        for (p, o) in bunch_iter(subject, cluster, dialect) {
            if !by.contains_key(p.as_str()) {
                preds.push(p.as_str());
                by.insert(p.as_str(), Vec::new());
            }
            by.get_mut(p.as_str()).unwrap().push(o);
        }
        preds.sort_by(|a, b| predicate_order(a, b));
        preds.into_iter().map(|p| (p, by.remove(p).unwrap())).collect()
    }

    // Whether one predicate covers the whole cluster with only plain objects —
    // written `[ p o1, o2 ]` on one line.
    fn is_compact(cluster: &[(String, JNode)]) -> bool {
        let mut pred: Option<&str> = None;
        for (p, o) in cluster {
            if matches!(o, JNode::Bnode(_)) {
                return false;
            }
            match pred {
                Some(q) if q != p => return false,
                _ => pred = Some(p),
            }
        }
        true
    }

    struct W<'a> {
        graph: &'a JenaGraph,
        dialect: Dialect,
        sep: &'static str,
        pred_str: &'a dyn Fn(&str) -> String,
        node_str: &'a dyn Fn(&JNode) -> String,
    }

    impl W<'_> {
        fn predicate_width(&self, groups: &[(&str, Vec<&JNode>)]) -> usize {
            groups
                .iter()
                .map(|(p, _)| (self.pred_str)(p).chars().count())
                .filter(|&w| w <= LONG_PREDICATE)
                .max()
                .unwrap_or(0)
                .max(MIN_PREDICATE)
        }

        /// Write one predicate, aligned; returns after the P→O gap (or the
        /// newline a long predicate takes instead).
        fn write_predicate(&self, out: &mut Out, p: &str, width: usize, first: bool) {
            if !first {
                out.print(self.sep);
                out.println();
            }
            let start = out.indent;
            let ps = (self.pred_str)(p);
            out.print(&ps);
            let w = out.col - start;
            if w > LONG_PREDICATE {
                out.println();
            } else {
                out.pad_to(width);
                out.gap(GAP_P_O);
            }
        }

        /// Write a nested `[ … ]` block for a blank node's own cluster.
        fn write_nested(&self, out: &mut Out, b: &JNode) {
            let cluster = &self.graph.clusters[b];
            if is_compact(cluster) {
                out.print("[ ");
                let indent0 = out.indent;
                out.indent += 2;
                self.write_pol(out, b, cluster);
                out.indent = indent0;
                out.print(" ]");
                return;
            }
            let indent0 = out.indent;
            out.indent = out.col;
            out.print("[ ");
            out.indent += 2;
            self.write_pol(out, b, cluster);
            out.indent -= 2;
            out.println();
            out.print("]");
            out.indent = indent0;
        }

        /// The predicate-object list of one cluster, at the current indent.
        fn write_pol(&self, out: &mut Out, subject: &JNode, cluster: &[(String, JNode)]) {
            let groups = group(subject, cluster, self.dialect);
            let width = self.predicate_width(&groups);
            let mut first = true;
            for (p, objs) in &groups {
                let (mut lits, mut simples, mut complexes) =
                    (Vec::new(), Vec::new(), Vec::new());
                for o in objs {
                    match o {
                        JNode::Lit { .. } => lits.push(*o),
                        JNode::Bnode(_) => complexes.push(*o),
                        JNode::Iri(_) => simples.push(*o),
                    }
                }
                for list in [&mut lits, &mut simples] {
                    if list.is_empty() {
                        continue;
                    }
                    self.write_predicate(out, p, width, first);
                    first = false;
                    out.indent += INDENT_OBJECT;
                    let mut first_obj = true;
                    for o in list.iter() {
                        if !first_obj {
                            out.print(" , ");
                        }
                        first_obj = false;
                        out.print(&(self.node_str)(o));
                    }
                    out.indent -= INDENT_OBJECT;
                }
                for o in complexes {
                    self.write_predicate(out, p, width, first);
                    first = false;
                    out.indent += INDENT_OBJECT;
                    self.write_nested(out, o);
                    out.indent -= INDENT_OBJECT;
                }
            }
        }
    }

    let w = W { graph, dialect, sep, pred_str: &pred_str, node_str: &node_str };

    let mut wrote_any = false;
    for &si in &order {
        let subject = &graph.subjects[si];
        // A blank node written inline under its referencing triple is not a
        // top-level cluster.
        if matches!(subject, JNode::Bnode(_)) {
            continue;
        }
        // One blank line between clusters: the previous cluster's ` .` line
        // already ended with a newline.
        if wrote_any {
            out.buf.push('\n');
            out.col = 0;
        }
        wrote_any = true;
        let cluster = &graph.clusters[subject];
        let s = node_str(subject);
        out.indent = 0;
        out.at_line_start = false;
        out.print(&s);
        if out.col > LONG_SUBJECT {
            out.println();
        } else {
            out.gap(GAP_P_O);
        }
        out.indent = INDENT_PREDICATE;
        out.pad_to(0);
        w.write_pol(&mut out, subject, cluster);
        out.indent = 0;
        out.print(" .");
        out.println();
    }
    Some(out.buf.into_bytes())
}
