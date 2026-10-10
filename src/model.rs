//! Core ontology model for owlmake.
//!
//! We standardize on horned-owl's reference-counted concrete types (`RcStr`)
//! and use [`SetOntology`] as the canonical in-memory representation, since it
//! is a simple set of [`AnnotatedComponent`]s that every other horned-owl
//! ontology type converts to and from.

use horned_owl::curie::PrefixMapping;
use horned_owl::model::{AnnotatedComponent, Build, RcAnnotatedComponent, RcStr};
use horned_owl::ontology::component_mapped::ComponentMappedOntology;
use horned_owl::ontology::set::SetOntology;

/// `xsd:boolean`. The datatype that makes `owl:deprecated` mean deprecation:
/// a TYPED boolean marks it, while an untyped `"true"` — or one carrying a
/// language tag — is a string that happens to spell it and marks nothing.
/// Defined once because three separate readers ask the same question.
pub const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";

/// The concrete IRI backing type used throughout owlmake.
pub type Str = RcStr;

/// Canonical in-memory ontology: a set of annotated components.
pub type Onto = SetOntology<RcStr>;

/// Component-mapped ontology, required by horned-owl's serializers.
pub type CmOnto = ComponentMappedOntology<RcStr, RcAnnotatedComponent>;

/// A loaded document as a functional write's banners see it.
#[derive(Clone, Debug)]
pub struct BannerDoc {
    pub iri: Option<String>,
    pub version: Option<String>,
    pub labels: std::sync::Arc<std::collections::HashMap<String, DocLabel>>,
    /// The document that opened the pipeline, whose identity is the one it
    /// carries when written.
    pub root: bool,
}

/// The label a document gives an entity (see [`crate::io::entities::HeldLabel`]):
/// a literal's text, or an IRI value, which names the entity until a literal
/// does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocLabel {
    Literal(String),
    Iri(String),
}

impl DocLabel {
    /// The label as a label provider shows it: a literal's text, or the short
    /// form of an IRI.
    pub fn short_form(&self) -> String {
        match self {
            DocLabel::Literal(text) => text.clone(),
            DocLabel::Iri(iri) => crate::owlapi_hash::iri_short_form(iri),
        }
    }
}

/// An import of the closure inlined into a model: its IRI, the document it was
/// read from (a path, or the IRI itself when fetched), and whether the root
/// imports it directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSource {
    pub iri: String,
    pub path: Option<std::path::PathBuf>,
    pub direct: bool,
}

/// The entities of an ontology's imports closure: every ontology it imports,
/// directly or not, leaving out the ontology itself. A document is written
/// among them:
///
/// - RDF/XML and Turtle state the type of an entity the document names and
///   nothing declares, unless an imported ontology has the entity in its
///   signature and so declares it on the document's behalf;
/// - functional syntax and OWL/XML declare every entity of the document's
///   signature and the closure's that neither declares;
/// - RDF/XML binds a namespace prefix for every annotation property of the
///   closure's signature, as for the document's own, since a property is
///   written as an element name.
///
/// Entities are keyed `kind\0IRI` (`class`, `op`, `dp`, `ap`, `ni`, `dt`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportsClosure {
    /// Every entity in the signature of an imported ontology, the datatype of
    /// each of its literals included.
    pub signature: std::collections::HashSet<String>,
    /// The entities an imported ontology declares.
    pub declared: std::collections::HashSet<String>,
    /// Where each ontology of the closure was read from, in the order they were
    /// read — what a writer that renders every ontology of the closure on its
    /// own reads them back from.
    pub documents: Vec<ImportSource>,
}

impl ImportsClosure {
    /// The closure whose ontologies are merged in `imported`, read from where
    /// `imported` records.
    pub fn of(imported: &Model) -> Self {
        let mut closure = ImportsClosure::default();
        closure.add_entities(imported);
        closure.documents = imported.import_sources.clone();
        closure
    }

    /// Add an imported ontology, read from `source`.
    pub fn add(&mut self, source: ImportSource, imported: &Model) {
        self.add_entities(imported);
        self.documents.push(source);
    }

    fn add_entities(&mut self, imported: &Model) {
        use crate::io::entities::{closure_key, declared, signature};
        self.signature.extend(signature(imported).into_iter().map(|(kind, iri)| closure_key(kind, &iri)));
        self.declared.extend(declared(imported).into_iter().map(|(kind, iri)| closure_key(kind, &iri)));
    }

    /// The namespaces of the closure's annotation properties, split as an
    /// RDF/XML element name is.
    pub fn annotation_property_namespaces(&self) -> impl Iterator<Item = String> + '_ {
        self.signature
            .iter()
            .filter_map(|key| key.strip_prefix("ap\0"))
            .map(|iri| crate::io::owlrdf::ncname_split(iri).0.to_string())
    }
}

/// One recorded node of an axiom (`Model::shared_occurrences`): an anonymous
/// expression in it that is one object with every other recorded occurrence
/// of the same node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SharedNode {
    /// The node's structure: the `anon_sig_hash` of its signature.
    pub sig: u64,
    /// The shared object the node belongs to — the merged class whose
    /// defining expression it is, or lies inside.
    pub group: u64,
    /// Whether the node lies inside that object rather than being it.
    pub inside: bool,
}

impl SharedNode {
    /// The group the node itself is numbered under: the object's own group, or
    /// for a node inside it, that group mixed with the node's structure — the
    /// group every occurrence of that node inside that object shares.
    pub fn node_group(&self) -> u64 {
        if self.inside {
            descendant_group(self.group, self.sig)
        } else {
            self.group
        }
    }
}

/// The group of the node with structure `sig` inside the shared object of
/// group `group`.
pub fn descendant_group(group: u64, sig: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (group, sig, "inside").hash(&mut h);
    h.finish()
}

/// An ontology together with the prefix/namespace mapping used to render it.
///
/// This is the value that flows between commands in a pipeline.
pub struct Model {
    pub ont: Onto,
    pub prefixes: PrefixMapping,
    pub build: Build<RcStr>,
    /// External `entity IRI → label` overrides for the functional-syntax
    /// `# Class: … (label)` banner comments — used when the ontology itself
    /// carries no `rdfs:label` for an entity (e.g. `mint`, which serialises the
    /// edit file but resolves banner labels from its import closure). Empty for
    /// ordinary models.
    pub banner_labels: std::collections::HashMap<String, String>,
    /// Every document this pipeline has loaded, as the banners of a functional
    /// write see them: the document that opened the pipeline, each one merged
    /// into it and each one its closure named, with the label each gives an
    /// entity. A write banners an entity with the label of the first document
    /// that has one, in the order the set of loaded documents is iterated in.
    /// Empty until a pipeline that writes functional syntax starts.
    pub banner_docs: Vec<BannerDoc>,
    /// Whether an OBO write of this model refuses a frame that carries a
    /// single-valued tag twice. On by default; `convert --check false` turns it
    /// off for the document it writes.
    pub obo_structure_check: bool,
    /// Per owning entity, the FNV-1a hashes of the anonymous class-expression
    /// signatures that shared ONE blank node in the RDF/XML this model came from.
    ///
    /// `rdf:nodeID` vs inline is decided by blank-node IDENTITY in the source, not
    /// by structural equality: reading `reasoned.owl` gives ONE expression object
    /// referenced by two axioms, which re-emits as one node — while the same axioms
    /// written as OFN text parse to two distinct objects and get two nodes. owlmake
    /// passes an OFN cache between build steps and OFN cannot express that identity,
    /// so this field carries it across.
    pub shared_anon: std::collections::HashMap<String, std::collections::HashSet<u64>>,
    /// Which anonymous expressions shared ONE blank node in the RDF/XML write
    /// this model most recently went through.
    ///
    /// OFN cannot express that two anonymous expressions were one node, and the
    /// `rdf:nodeID`-vs-inline choice depends on exactly that, so the fact has to
    /// travel from an RDF/XML write to the OFN cache written right after it. Carried
    /// on the model it describes, it travels exactly as far as the model does —
    /// through a thread-local it would couple one write's output to whether an
    /// earlier write happened to run on the same thread, so the same plan could
    /// produce different bytes depending on execution order.
    pub rdf_shared_anon: std::collections::HashMap<String, std::collections::HashSet<u64>>,
    /// `owl:imports` IRIs whose closure was inlined into this model.
    ///
    /// Reasoning runs over the loaded closure, but the root ontology is SERIALISED
    /// with its import declarations intact, so a `reason -o` output still
    /// imports. Recording the IRIs here lets a writer restore them rather than
    /// emitting a silently self-contained document.
    pub inlined_imports: Vec<String>,
    /// Where each inlined import was read from, in the order they were
    /// resolved — what a command that keeps every document of the closure as a
    /// graph of its own reads them back from.
    pub import_sources: Vec<ImportSource>,
    /// The components that inlining the closure CONTRIBUTED — every component an
    /// import added that the root did not already assert.
    ///
    /// The other half of `inlined_imports`, and inseparable from it. A command
    /// works over the whole closure and writes the root ontology, so the axioms
    /// the closure lent it are removed again on save: restoring the import
    /// declarations without removing them would write a document that both
    /// inlines its imports and still imports them, freezing one version of an
    /// import into a file that also tells its consumer to load whatever the IRI
    /// resolves to later.
    ///
    /// A command whose product is a NEW ontology built out of the closure —
    /// `merge`, `extract`, `subset` — says so with
    /// [`Model::detach_import_closure`], and then what came from an import is its
    /// own content and is written.
    pub imported_components: std::collections::HashSet<AnnotatedComponent<RcStr>>,
    /// Whether this model came from a source that EXPRESSES blank-node identity
    /// at all — an RDF/XML document, where two occurrences are one node only if
    /// the document says so with an `rdf:nodeID`.
    ///
    /// Distinct from `owl_shared_owners` being non-empty, and the distinction is
    /// load-bearing. That map is the list of owners which DID share; reading its
    /// emptiness as "this source records nothing" conflates an RDF/XML document
    /// that simply shares no node with an OBO or functional one that cannot
    /// express sharing in the first place. The first must keep its expressions
    /// separate; only the second may fall back to structural equality.
    ///
    /// EFO's `mondo_import.owl` is the case. `robot remove` reads an RDF/XML
    /// module in which nothing is shared, so the map came out empty, the
    /// permissive rule applied, and four `predisposes_towards` restrictions
    /// belonging to two DIFFERENT classes (MONDO_0013920 and MONDO_0013921 —
    /// four distinct `rdf:nodeID`s in the source) collapsed into one shared
    /// node: 159 restrictions where ROBOT writes 163, and 1,032 bytes that
    /// propagated into `efo.owl` and `efo.obo`.
    pub rdf_blank_node_identity: bool,
    /// Classes whose RDF/XML source referenced one `rdf:nodeID` twice in the same
    /// body — positive evidence that two structurally-equal anonymous expressions
    /// really are ONE blank node. Absent that evidence, each occurrence is a node
    /// of its own.
    pub owl_shared_owners:
        std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// The import IRIs in document order. The in-memory ontology is an unordered
    /// set, so the source file's `Import(...)` order is recorded here, and the
    /// functional-syntax writer emits its `Import(...)` lines in this order. Empty
    /// when the order is unknown.
    pub import_order: Vec<String>,
    /// `idspace:` declarations to emit in OBO output — the non-builtin prefixes
    /// (`prefix`, `namespace`) of the source document's prefix map, listed whether
    /// or not any id is shortened with them. Populated from the raw document at read
    /// time (RDF/XML has no formal prefix map so it is scanned). Empty when the
    /// source is not an OWL document (an obo→obo trip keeps its own).
    pub idspaces: Vec<(String, String)>,
    /// Prefixes an OBO rendering declares with an `idspace:` line whether or
    /// not an id is shortened with them: an OBO source's own `idspace:` lines,
    /// the prefixes the command line adds to a cleaned OBO write
    /// (`convert --clean-obo`), and a build's `convert --add-prefixes` context,
    /// so mondo's `config/prefixes.jsonld` yields `idspace: ICD11` even with
    /// zero ICD11 references. Recorded apart from the CURIE map, which also
    /// binds prefixes no document declared.
    pub explicit_prefixes: Vec<(String, String)>,
    /// Every `xmlns:PREFIX="NS"` declaration from an RDF/XML source, in document
    /// order, including built-in prefixes (owl, rdf, rdfs, xsd, xml, obo, …) that
    /// `idspaces` filters out. This is the full prefix map the RDF/XML writer
    /// re-declares on `rdf:RDF`. Empty otherwise.
    pub rdf_prefixes: Vec<(String, String)>,
    /// Prefix bindings this ontology's CONSTRUCTION brought, for an ontology built
    /// from something that is not an OWL document.
    ///
    /// A model built from a table inherits no xmlns block
    /// (`format_prefixes_cleared`), but it is not therefore a document with no
    /// prefixes: the table's own CURIEs are bindings, and the document declares
    /// them. `babelon convert` is the case — `HP:0000001` binds
    /// `HP` to `http://purl.obolibrary.org/obo/HP_`, and the translation ontologies
    /// open `xmlns:HP="http://purl.obolibrary.org/obo/HP_"`. Only bindings the
    /// built-in namespaces do not already cover are recorded; the writer sorts the
    /// whole block by prefix length, so where they land is not this field's
    /// business.
    pub built_prefixes: Vec<(String, String)>,
    /// Prefixes the command line adds to the document it writes
    /// (`--add-prefix`, `--add-prefixes`). Every prefix-format writer — RDF/XML,
    /// functional syntax, OWL/XML, Manchester, Turtle — declares each one, used
    /// or not, over the source document's own, and abbreviates with it.
    pub added_prefixes: Vec<(String, String)>,
    /// Per class IRI, the `genidN` blank-node ids referenced by `rdf:nodeID` in
    /// the class body, in document order. The RDF/XML writer assigns them
    /// positionally to the class's annotated anonymous superclasses
    /// (rendered `rdf:nodeID="genidN"`, defined separately, reified) — the source
    /// document's blank-node numbering isn't reconstructible from horned's model.
    pub owl_genid_refs: std::collections::HashMap<String, Vec<String>>,
    /// Per subject IRI, the `rdfs:label` values in the order the source document
    /// carried them. Where two labels land in the same slot of the subject's
    /// assertion set, the one read first is the one the `! …` comments name.
    pub owl_label_order: std::collections::HashMap<String, Vec<String>>,
    /// The entities of the ontologies this one imports, directly or not, which
    /// decide how this one is written (see [`ImportsClosure`]); `None` until
    /// they have been read.
    pub imports_closure: Option<ImportsClosure>,
    /// Anonymous-individual node labels in the order the SOURCE DOCUMENT first
    /// mentions them. An anonymous individual is re-minted the first time it is
    /// asked for and the set renders sorted by the minted id, so for a
    /// functional-syntax document — where the parser meets them in document order
    /// — the rendered order IS document order. The model is a set and cannot
    /// recover that, so it is scanned off the text.
    pub anon_doc_order: Vec<String>,

    /// True when this model's untyped literals are `xsd:string` rather than
    /// `rdf:PlainLiteral` — which changes the RDF/XML writer's ordering.
    ///
    /// A literal with no datatype and no language carries one of two datatypes:
    /// `rdf:PlainLiteral` or `xsd:string`. Both render bare, but literals compare by
    /// DATATYPE IRI FIRST, then the lexical form, then the language — so which one
    /// is in play reorders a subject's triples. `rdf:PlainLiteral` is
    /// `…/1999/02/22-rdf-syntax-ns#PlainLiteral` and sorts before every `xsd:`
    /// datatype; `xsd:string` sorts after `xsd:anyURI`.
    ///
    /// On a class carrying `IAO_0000233 "…7189"` (untyped) and
    /// `IAO_0000233 "…9285"^^xsd:anyURI`: OFN, RDF/XML and the
    /// command chain (merge/reason/relax/reduce/filter) all keep `rdf:PlainLiteral`
    /// and emit `7189` first, while `query --update`, Turtle input and OBO input
    /// give `xsd:string` and emit `9285` first. `mondo-simple.owl`'s chain ends
    /// `… filter reduce query --update … annotate`, so its output takes the
    /// `xsd:string` ordering.
    pub plain_literals_typed: bool,
    /// RDF/XML write profile that renders every anonymous class expression
    /// inline at each place it is referenced — an annotated axiom's
    /// `owl:annotatedTarget` carries a full copy of the expression rather than a
    /// reference, and no `rdf:nodeID` appears anywhere in the document — and
    /// stamps the OWL API 4.5.6 banner. The owltools emulation
    /// ([`crate::cmd::owltools_ops`]) saves under this profile; every other save
    /// shares blank nodes between an annotated edge and its reification.
    pub owlapi_456: bool,
    /// The prefixes a CURIE the command line gives is read with
    /// ([`crate::context`]): never this document's own, which say only how it
    /// writes its IRIs.
    pub context: crate::context::Context,
    /// `owner\u{1}signature -> group` for superclass expressions that are ONE
    /// object asserted for several owners, rendered inline at each.
    ///
    /// Two steps produce that shape. `span_gaps` re-links the ontology's own
    /// expression object through a removed intermediate onto several retained
    /// subclasses; only signatures whose every occurrence traces to a single
    /// source expression are recorded, so two structurally-equal expressions from
    /// different sources stay distinct. `materialize` builds one `∃P.D` per
    /// (property, filler) and asserts it for every subclass that gets it.
    ///
    /// One object means ONE blank node however many classes carry it — and each
    /// entity references it once, so it still renders inline and only the
    /// numbering moves. That is what separates this from `cross_shared`, whose
    /// members render as `rdf:nodeID` references.
    pub span_shared: std::collections::HashMap<String, u64>,
    /// `owner\u{1}property\u{1}filler -> group` for blank nodes the SOURCE shared
    /// between several classes (see `io::scan_cross_owner_shared`).
    pub cross_shared: std::collections::HashMap<String, u64>,
    /// Per axiom (`genid::axiom_identity`), the class expressions in it that
    /// are ONE object with the other recorded occurrences of the same group:
    /// `(signature hash, group)`, the hash being `io::anon_sig_hash` of
    /// `genid::ce_sig`. The species merge substitutes a merged class's defining
    /// expression itself — the object its own equivalence held — into every
    /// axiom that named the class, so one anonymous node stands for it across
    /// the whole document: a graph that reaches it again re-spends only its
    /// list cells, and a graph that holds it twice spends nothing the second
    /// time. The group is the merged class, because two merged classes can
    /// define themselves by the same structure (two mouse ontologies, one
    /// taxon) and still be two objects. An occurrence not recorded here is a
    /// fresh object, however equal its structure: a later pass that rebuilds an
    /// axiom copies it, and a rename rewrites it under a new identity that no
    /// record names.
    pub shared_occurrences: std::collections::HashMap<u64, Vec<SharedNode>>,
    /// True when this model came out of a step that built a BRAND-NEW ontology,
    /// so its document format carries no prefixes at all.
    ///
    /// `filter` collects the retained axioms into a fresh ontology, and a fresh
    /// ontology starts with an empty document format — so its `rdf:RDF` xmlns block
    /// is rebuilt from the built-in namespaces plus the entity-derived ones alone. On
    /// MONDO's mondo-simple chain that shows up sharply: every step through
    /// `remove --select object-properties relax` still declares `xmlns:doap` and
    /// `xmlns:protege` — inherited from `reasoned.owl`, where the import closure
    /// contributed them — and the output of `filter` declares neither while keeping
    /// every other prefix, each of which some retained entity uses.
    ///
    /// An empty `rdf_prefixes` cannot express this on its own: it also means "no
    /// xmlns was scanned", which makes the writer fall back to `idspaces` and then
    /// to the CURIE map — and the CURIE map still holds `doap`/`protege`.
    pub format_prefixes_cleared: bool,
    /// Whether this model was read from an OBO document. An OBO document's only
    /// prefix declarations are its `idspace:` lines, so the OBO writer must not
    /// fall back to the pipeline's prefix map when re-serializing one — a
    /// prefix the document never declared must not curie its ids or earn an
    /// `idspace:` line.
    pub obo_source: bool,
    /// `convert --clean-obo drop-untranslatable-axioms` was asked for, so
    /// the OBO writer emits no `owl-axioms:` header.
    ///
    /// The flag throws the untranslatable remainder away instead of parking it in
    /// the header. That is not the same as deleting the axioms — an n-ary
    /// `DisjointClasses` is PARTIALLY translatable, and OBA's `oba.obo` carries
    /// its `disjoint_from:` clause while having no `owl-axioms:` line at all.
    pub obo_drop_untranslatable: bool,
}

impl Model {
    /// The natural order of this document's objects, in which its sets are
    /// stored: how its untyped literals key is [`Model::plain_literals_typed`].
    pub fn natural_order(&self) -> crate::io::natural_order::NaturalOrder {
        crate::io::natural_order::NaturalOrder::new(self.plain_literals_typed)
    }

    pub fn new() -> Self {
        Model {
            ont: SetOntology::new(),
            prefixes: default_prefixes(),
            build: Build::new(),
            banner_labels: std::collections::HashMap::new(),
            banner_docs: Vec::new(),
            obo_structure_check: true,
            shared_anon: std::collections::HashMap::new(),
            rdf_shared_anon: std::collections::HashMap::new(),
            inlined_imports: Vec::new(),
            import_sources: Vec::new(),
            imported_components: Default::default(),
            rdf_blank_node_identity: false,
            owl_shared_owners: std::collections::HashMap::new(),
            import_order: Vec::new(),
            idspaces: Vec::new(),
            explicit_prefixes: Vec::new(),
            rdf_prefixes: Vec::new(),
            built_prefixes: Vec::new(),
            added_prefixes: Vec::new(),
            owl_genid_refs: std::collections::HashMap::new(),
            owl_label_order: std::collections::HashMap::new(),
            imports_closure: None,
            anon_doc_order: Vec::new(),
            plain_literals_typed: false,
            owlapi_456: false,
            context: Default::default(),
            span_shared: std::collections::HashMap::new(),
            cross_shared: std::collections::HashMap::new(),
            shared_occurrences: std::collections::HashMap::new(),
            format_prefixes_cleared: false,
            obo_source: false,
            obo_drop_untranslatable: false,
        }
    }

    pub fn from_parts(ont: Onto, prefixes: PrefixMapping) -> Self {
        Model {
            ont,
            prefixes,
            build: Build::new(),
            banner_labels: std::collections::HashMap::new(),
            banner_docs: Vec::new(),
            obo_structure_check: true,
            shared_anon: std::collections::HashMap::new(),
            rdf_shared_anon: std::collections::HashMap::new(),
            inlined_imports: Vec::new(),
            import_sources: Vec::new(),
            imported_components: Default::default(),
            rdf_blank_node_identity: false,
            owl_shared_owners: std::collections::HashMap::new(),
            import_order: Vec::new(),
            idspaces: Vec::new(),
            explicit_prefixes: Vec::new(),
            rdf_prefixes: Vec::new(),
            built_prefixes: Vec::new(),
            added_prefixes: Vec::new(),
            owl_genid_refs: std::collections::HashMap::new(),
            owl_label_order: std::collections::HashMap::new(),
            imports_closure: None,
            anon_doc_order: Vec::new(),
            plain_literals_typed: false,
            owlapi_456: false,
            context: Default::default(),
            span_shared: std::collections::HashMap::new(),
            cross_shared: std::collections::HashMap::new(),
            shared_occurrences: std::collections::HashMap::new(),
            format_prefixes_cleared: false,
            obo_source: false,
            obo_drop_untranslatable: false,
        }
    }

    /// Copy document-level metadata (prefix bindings, the RDF/XML xmlns
    /// `rdf_prefixes`, explicit `--add-prefixes` set, idspaces, banner labels,
    /// import order, scanned owl-render hints) from `other`. Ops that REBUILD the
    /// ontology via `Model::from_parts` (reason/reduce/materialize/merge, …) must
    /// call this so the metadata a downstream writer needs — e.g. `rdf_prefixes`
    /// for owlrdf's xmlns block, `explicit_prefixes` for OBO idspaces — is not
    /// silently dropped mid-pipeline.
    pub fn carry_meta_from(&mut self, other: &Model) {
        self.banner_labels = other.banner_labels.clone();
        self.banner_docs = other.banner_docs.clone();
        self.obo_structure_check = other.obo_structure_check;
        self.shared_anon = other.shared_anon.clone();
        self.rdf_shared_anon = other.rdf_shared_anon.clone();
        self.inlined_imports = other.inlined_imports.clone();
        self.import_sources = other.import_sources.clone();
        self.imported_components = other.imported_components.clone();
        self.rdf_blank_node_identity = other.rdf_blank_node_identity;
        self.owl_shared_owners = other.owl_shared_owners.clone();
        self.import_order = other.import_order.clone();
        self.idspaces = other.idspaces.clone();
        self.rdf_prefixes = other.rdf_prefixes.clone();
        self.built_prefixes = other.built_prefixes.clone();
        self.added_prefixes = other.added_prefixes.clone();
        self.explicit_prefixes = other.explicit_prefixes.clone();
        self.owl_genid_refs = other.owl_genid_refs.clone();
        self.owl_label_order = other.owl_label_order.clone();
        self.imports_closure = other.imports_closure.clone();
        self.anon_doc_order = other.anon_doc_order.clone();
        self.plain_literals_typed = other.plain_literals_typed;
        self.owlapi_456 = other.owlapi_456;
        self.context = other.context.clone();
        self.span_shared = other.span_shared.clone();
        self.cross_shared = other.cross_shared.clone();
        self.shared_occurrences = other.shared_occurrences.clone();
        self.format_prefixes_cleared = other.format_prefixes_cleared;
        self.obo_source = other.obo_source;
        self.obo_drop_untranslatable = other.obo_drop_untranslatable;
    }

    /// Declare that this model is a NEW ontology in its own right, not the root
    /// of an import closure — so what the closure contributed is now its own
    /// content, written like everything else, and no `Import(...)` declaration is
    /// restored on save.
    ///
    /// This is what separates a module from a processed root. `merge`, `extract`
    /// and `subset` each build one document out of a closure and are complete
    /// without it; `reason` and its kind hand back the ontology they were given
    /// and are not.
    pub fn detach_import_closure(&mut self) {
        self.inlined_imports.clear();
        self.imported_components.clear();
        self.import_sources.clear();
        // The closure's entities described a document that still imported;
        // once the closure's axioms are the document's own, an entity the
        // closure declared is declared HERE, and suppressing its stub or its
        // annotations hides content the document now carries.
        self.imports_closure = None;
    }

    /// Whether an ontology this one imports has the entity keyed `key`
    /// (`kind\0IRI`, see [`ImportsClosure`]) in its signature.
    pub fn imports_have(&self, key: &str) -> bool {
        self.imports_closure.as_ref().is_some_and(|c| c.signature.contains(key))
    }

    /// Number of components (axioms + metadata) in the ontology.
    pub fn len(&self) -> usize {
        self.ont.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.ont.iter().next().is_none()
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Model {
    /// Deep-clones the ontology and prefix map. (A derived `Clone` is not
    /// possible because curie's `PrefixMapping` is not `Clone`; the `build`
    /// IRI interner is reconstructed — it is a cache, so this is semantically a
    /// no-op.)
    fn clone(&self) -> Self {
        let mut m = Model::from_parts(self.ont.clone(), clone_prefixes(&self.prefixes));
        m.banner_labels = self.banner_labels.clone();
        m.banner_docs = self.banner_docs.clone();
        m.obo_structure_check = self.obo_structure_check;
        m.shared_anon = self.shared_anon.clone();
        m.rdf_shared_anon = self.rdf_shared_anon.clone();
        m.inlined_imports = self.inlined_imports.clone();
        m.import_sources = self.import_sources.clone();
        m.imported_components = self.imported_components.clone();
        m.owl_shared_owners = self.owl_shared_owners.clone();
        m.import_order = self.import_order.clone();
        m.idspaces = self.idspaces.clone();
        m.rdf_prefixes = self.rdf_prefixes.clone();
        m.built_prefixes = self.built_prefixes.clone();
        m.added_prefixes = self.added_prefixes.clone();
        m.explicit_prefixes = self.explicit_prefixes.clone();
        m.owl_genid_refs = self.owl_genid_refs.clone();
        m.owl_label_order = self.owl_label_order.clone();
        m.imports_closure = self.imports_closure.clone();
        m.anon_doc_order = self.anon_doc_order.clone();
        m.plain_literals_typed = self.plain_literals_typed;
        m.owlapi_456 = self.owlapi_456;
        m.context = self.context.clone();
        m.span_shared = self.span_shared.clone();
        m.cross_shared = self.cross_shared.clone();
        m.shared_occurrences = self.shared_occurrences.clone();
        m.format_prefixes_cleared = self.format_prefixes_cleared;
        m.obo_drop_untranslatable = self.obo_drop_untranslatable;
        m
    }
}

impl std::fmt::Debug for Model {
    /// A summary (component and prefix counts); the full component set is far too
    /// large to print. Lets `Model` be used with `dbg!`, `assert_eq!` context,
    /// and `#[derive(Debug)]` on types that contain it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("components", &self.ont.iter().count())
            .field("prefixes", &self.prefixes.mappings().count())
            .finish_non_exhaustive()
    }
}

/// Deep-clone a prefix mapping (curie's `PrefixMapping` is not `Clone`).
pub fn clone_prefixes(p: &PrefixMapping) -> PrefixMapping {
    let mut out = PrefixMapping::default();
    for (k, v) in p.mappings() {
        let _ = out.add_prefix(k, v);
    }
    out
}

/// The standard prefix map shared by OBO-family ontologies.
pub fn default_prefixes() -> PrefixMapping {
    let mut p = PrefixMapping::default();
    let _ = p.add_prefix("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#");
    let _ = p.add_prefix("rdfs", "http://www.w3.org/2000/01/rdf-schema#");
    let _ = p.add_prefix("xsd", "http://www.w3.org/2001/XMLSchema#");
    let _ = p.add_prefix("owl", "http://www.w3.org/2002/07/owl#");
    // `dc` is dc/elements/1.1/ HERE, which is what documents declare and what the
    // OBO writer's `idspace:` table and the RDF/XML xmlns block need. A CURIE a
    // command is given is read with the command line's context, which binds `dc`
    // to dc/TERMS/ instead — see [`crate::context`].
    // The two are genuinely different maps: binding this one to dc/terms/ would
    // shadow the elements/1.1/ namespace, dropping MONDO's `idspace: dc` line and
    // every `dc:date`/`dc:title` abbreviation in `mondo.obo`.
    let _ = p.add_prefix("dc", "http://purl.org/dc/elements/1.1/");
    // `terms` is the name http://purl.org/dc/terms/ takes when the source declares
    // no prefix of its own: the namespace is in neither the OBO context map nor the
    // built-ins (owl/rdfs/rdf/xsd/dc/skos), so a prefix is *generated* from the
    // trailing NCName run, giving exactly `terms`.
    //
    // `dcterms` is deliberately NOT seeded. It is in play only when a document
    // declares it, and when a document does, it legitimately WINS: a file
    // declaring both renders `dcterms:license` and lists both idspaces. That is
    // what the (namespace len, prefix len, prefix) tie-break in `io::obo`
    // decides, and MONDO's own `config/prefixes.jsonld` relies on it for the
    // `ICD10CM`/`icd10cm` and `ICD11`/`icd11.foundation` aliases. Seeding
    // `dcterms` here would hand that tie-break a prefix no document supplied, so
    // MONDO — which declares only `terms`, via `imports/omo_import.owl` — would
    // render 4,473 `property_value:` lines as `dcterms:` and emit
    // `idspace: dcterms`. A document that declares `dcterms` still gets it from
    // its own prefix map.
    let _ = p.add_prefix("terms", "http://purl.org/dc/terms/");
    let _ = p.add_prefix("oboInOwl", "http://www.geneontology.org/formats/oboInOwl#");
    let _ = p.add_prefix("obo", "http://purl.obolibrary.org/obo/");
    p
}

/// The IRI of `owl:deprecated`.
pub const OWL_DEPRECATED: &str = "http://www.w3.org/2002/07/owl#deprecated";

/// Whether an annotation value asserts deprecation.
///
/// Deprecation is the typed boolean `true`. An untyped `"true"`, or one carrying
/// a language tag, is a string that happens to spell the word and marks nothing —
/// so a term annotated that way is live, and every code path that asks whether a
/// term is obsolete gets the same answer from this one predicate.
pub fn asserts_deprecated(av: &horned_owl::model::AnnotationValue<Str>) -> bool {
    use horned_owl::model::{AnnotationValue, Literal};
    matches!(
        av,
        AnnotationValue::Literal(Literal::Datatype { literal, datatype_iri })
            if literal == "true"
                && datatype_iri.as_ref() == "http://www.w3.org/2001/XMLSchema#boolean"
    )
}

/// An ontology or version IRI as an ontology's ID holds it. One that is not
/// absolute is made so by prefixing `urn:absolute:`, and logged as an error;
/// one that labels a blank node, `_:` with `genid` somewhere after it, names no
/// IRI.
pub(crate) fn ontology_iri_as_made(build: &Build<RcStr>, iri: &str) -> Option<horned_owl::model::IRI<RcStr>> {
    if iri.starts_with("_:") && iri.contains("genid") {
        return None;
    }
    if horned_owl::model::is_absolute_iri(iri) {
        return Some(build.iri(iri));
    }
    crate::cmd::reason::log_error(
        "org.semanticweb.owlapi.model.OWLOntologyID",
        &format!(
            "Ontology IRIs must be absolute; IRI {iri} is relative and will be made absolute by prefixing urn:absolute: to it"
        ),
    );
    Some(build.iri(format!("urn:absolute:{iri}")))
}

/// The ID of an ontology with the given IRI and version IRI, each as
/// [`ontology_iri_as_made`] makes it. A version IRI with no ontology IRI is
/// refused.
pub(crate) fn ontology_id(
    build: &Build<RcStr>,
    iri: Option<&str>,
    viri: Option<&str>,
) -> anyhow::Result<horned_owl::model::OntologyID<RcStr>> {
    let id = horned_owl::model::OntologyID {
        iri: iri.and_then(|iri| ontology_iri_as_made(build, iri)),
        viri: viri.and_then(|viri| ontology_iri_as_made(build, viri)),
    };
    if id.iri.is_none() && id.viri.is_some() {
        anyhow::bail!("If the ontology IRI is null then it is not possible to specify a version IRI");
    }
    Ok(id)
}

/// A literal as an ontology holds it once made: `l` itself, or what
/// [`remade_literal`] makes of it.
pub fn literal_as_made(l: horned_owl::model::Literal<Str>) -> horned_owl::model::Literal<Str> {
    remade_literal(&l).unwrap_or(l)
}

/// What making `l` turns it into, where that is another literal.
///
/// A language tag is trimmed and lower-cased, and a literal whose tag is then
/// empty is plain. `rdf:PlainLiteral` text names its language after its last
/// `@`, kept as written, and is plain where nothing follows the `@` or there is
/// none. An `xsd:boolean` is `true` for `1` or `true`, trimmed, and `false`
/// for anything else. An `xsd:float` or `xsd:double` is its value as Java
/// prints it, a float that trims to `-0.0` being `-0.0`. An `xsd:integer` is
/// its value printed, unless it is blank or starts with `0` once trimmed. Text
/// a number type cannot read keeps its form, as does every other literal.
pub fn remade_literal(l: &horned_owl::model::Literal<Str>) -> Option<horned_owl::model::Literal<Str>> {
    use crate::java_number as java;
    use horned_owl::model::Literal;
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
    const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
    match l {
        Literal::Simple { .. } => None,
        Literal::Language { literal, lang } => {
            let made = java::trim(lang).to_lowercase();
            if made == *lang {
                None
            } else if made.is_empty() {
                Some(Literal::Simple { literal: literal.clone() })
            } else {
                Some(Literal::Language { literal: literal.clone(), lang: made })
            }
        }
        Literal::Datatype { literal, datatype_iri } => {
            let datatype: &str = datatype_iri.as_ref();
            if datatype == RDF_PLAIN_LITERAL {
                return Some(match literal.rfind('@') {
                    Some(at) if at + 1 < literal.len() => {
                        Literal::Language { literal: literal[..at].to_string(), lang: literal[at + 1..].to_string() }
                    }
                    Some(at) => Literal::Simple { literal: literal[..at].to_string() },
                    None => Literal::Simple { literal: literal.clone() },
                });
            }
            let made = match datatype.strip_prefix(XSD)? {
                "boolean" => Some((if matches!(java::trim(literal), "1" | "true") { "true" } else { "false" }).to_string()),
                "float" if java::trim(literal) == "-0.0" => Some("-0.0".to_string()),
                "float" => java::parse_float(literal).map(java::float_to_string),
                "double" => java::parse_double(literal).map(java::double_to_string),
                "integer" => {
                    let t = java::trim(literal);
                    if t.is_empty() || t.starts_with('0') {
                        None
                    } else {
                        java::parse_int(literal).map(|i| i.to_string())
                    }
                }
                _ => None,
            }?;
            (made != *literal).then(|| Literal::Datatype { literal: made, datatype_iri: datatype_iri.clone() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horned_owl::model::Literal;

    /// Every case `scripts/gen_owlapi_literals.sh` recorded in
    /// tests/fixtures/owlapi-literals/cases.tsv: a lexical form, the datatype or
    /// language tag it is made with, and the lexical form, language and
    /// datatype of the literal made.
    #[test]
    fn literals_are_made_as_recorded() {
        const PLAIN: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/owlapi-literals/cases.tsv");
        let text = std::fs::read_to_string(path).unwrap();
        // The fixture writes a character as `\u` and four hex digits of UTF-16.
        let unescape = |s: &str| -> String {
            let mut units = Vec::new();
            let mut rest = s;
            while let Some(i) = rest.find("\\u") {
                units.extend(rest[..i].encode_utf16());
                units.push(u16::from_str_radix(&rest[i + 2..i + 6], 16).unwrap());
                rest = &rest[i + 6..];
            }
            units.extend(rest.encode_utf16());
            String::from_utf16(&units).unwrap()
        };
        let b = Build::new_rc();
        let mut cases = 0;
        let mut wrong = Vec::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.split('\t').collect();
            let (lex, with, want) = (unescape(f[0]), unescape(f[1]), (unescape(f[2]), unescape(f[3]), f[4].to_string()));
            let made = literal_as_made(match with.strip_prefix('@') {
                Some(lang) => Literal::Language { literal: lex.clone(), lang: lang.to_string() },
                None => Literal::Datatype { literal: lex.clone(), datatype_iri: b.iri(with.clone()) },
            });
            let got = match made {
                Literal::Simple { literal } => (literal, String::new(), PLAIN.to_string()),
                Literal::Language { literal, lang } => (literal, lang, PLAIN.to_string()),
                Literal::Datatype { literal, datatype_iri } => (literal, String::new(), datatype_iri.to_string()),
            };
            cases += 1;
            if got != want {
                wrong.push(format!("{lex:?} {with}: made {got:?}, recorded {want:?}"));
            }
        }
        assert!(cases > 2000, "{cases} cases");
        assert!(wrong.is_empty(), "{} of {cases} differ:\n{}", wrong.len(), wrong.join("\n"));
    }
}
