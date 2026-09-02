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

/// One verbatim `<rdf:Description>` block, with the position of the blank node
/// allocated for it as the document is read.
///
/// The position is document-relative; `Model::anon_alloc_base` carries the rest,
/// and `offset` says whether this block is even entitled to it — see
/// `Model::anon_imports_end`.
#[derive(Clone, Debug)]
pub struct AnonBlock {
    /// Byte offset of the block in its source document.
    pub offset: u64,
    /// How many blank nodes this document allocated BEFORE this block's own.
    pub alloc: u64,
    /// The block, exactly as the source wrote it.
    pub text: String,
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
    /// Prefixes loaded from an explicit `--prefixes`/`--add-prefixes` context file.
    /// Unlike the built-in/default prefix map, EVERY explicitly-provided prefix gets
    /// an `idspace:` line when writing OBO — regardless of whether it is used to
    /// shorten an id — so mondo's `config/prefixes.jsonld` yields e.g.
    /// `idspace: ICD11` even with zero ICD11 references. Recorded separately so the
    /// OBO writer can tell them from the default map.
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
    /// Namespaces of the annotation properties in this ontology's IMPORT CLOSURE.
    ///
    /// The `xmlns` block is seeded from the entities that need a namespace across
    /// the WHOLE closure, not just the root document: an ontology that merely
    /// *imports* one declaring
    /// `AnnotationProperty(<http://usefulinc.com/ns/doap#bug-database>)` still gets
    /// `xmlns:doap`, while an imported ObjectProperty/Class/Individual/Datatype in
    /// its own namespace gets nothing (only annotation properties are rendered as
    /// XML element names). MONDO's `filtered.owl`/`reasoned.owl` keep their imports
    /// uncollapsed, so `doap` (merged_import.owl) and `protege` (omo_import.owl)
    /// reach the xmlns block — and from there every downstream artefact's prefix
    /// map — without a single triple in the file using them.
    pub closure_ann_ns: Vec<String>,
    /// Entities DECLARED in this ontology's import closure, as `kind\0IRI`
    /// (`class`, `op`, `dp`, `ap`, `ni`, `dt`).
    ///
    /// The RDF/XML writer drives its per-kind sections from the SIGNATURE, so
    /// an entity that is only referenced still gets a bare
    /// `<owl:ObjectProperty rdf:about="…"/>` stub — but ONLY if nothing in the
    /// imports closure declares it. An ontology referencing an undeclared
    /// `BFO_0000050` renders the stub; add an import that
    /// declares it (uncollapsed) and the stub disappears. That single rule explains
    /// why MONDO's `filtered.owl`/`reasoned.owl` have no stubs while
    /// `mondo-base.owl` — built by `remove --select imports` — has exactly two.
    pub closure_declared: std::collections::HashSet<String>,
    /// Verbatim bare `<rdf:Description>` blocks (anonymous-individual annotation
    /// assertions — EFO obsolescence records) scanned from the source, which
    /// horned's RDF reader discards. Passed through unchanged in the Individuals
    /// section. Empty otherwise.
    pub owl_anon_blocks: Vec<AnonBlock>,
    /// Blank nodes the documents merged INTO this one consumed from the global
    /// blank-node counter before this document's own parse reached its blocks.
    ///
    /// An `owl:imports` is loaded at the moment its triple streams past, and an
    /// ontology header sits at the top of the file — so by the time the parse
    /// reaches anything else, every import has been read and has taken its share of
    /// the counter. That offset is what decides the ORDER the anonymous
    /// individuals come out in (see `io::anon_individual_order`), so it has to
    /// accumulate as the closure is merged.
    pub anon_alloc_base: u64,
    /// How many blank nodes THIS document's own parse consumed. Added to a merge
    /// target's `anon_alloc_base`, since the counter is global across the closure
    /// and every import is parsed before the importing document's body is reached.
    pub anon_alloc_total: u64,

    /// The capacity of the hash table whose iteration order decides which anonymous
    /// individual is re-minted first — the table of literal triples keyed by
    /// subject, sized by how many distinct subjects ever carried a literal triple
    /// (start 16, load factor 0.75, double on overflow). It is a property of the
    /// DOCUMENT, like `anon_alloc_total`, so it travels with the model rather than
    /// being re-derived at write time. Zero means "not an RDF/XML source", and the
    /// writer falls back to a mask above any real count.
    pub anon_hash_capacity: u64,

    /// Anonymous-individual node labels in the order the SOURCE DOCUMENT first
    /// mentions them. An anonymous individual is re-minted the first time it is
    /// asked for and the set renders sorted by the minted id, so for a
    /// functional-syntax document — where the parser meets them in document order
    /// — the rendered order IS document order. The model is a set and cannot
    /// recover that, so it is scanned off the text like the RDF/XML counts.
    pub anon_doc_order: Vec<String>,

    /// Byte offset just past the document's LAST `owl:imports`, or 0 when it
    /// declares none.
    ///
    /// An import is loaded at the moment its triple streams past, so the closure's
    /// blank nodes are allocated at that point — not unconditionally before the
    /// document's own. A node allocated EARLIER in the document than the imports
    /// declaration is numbered without them. Every real ontology puts its header
    /// first, so this is all-or-nothing in practice; recorded because the
    /// alternative is being silently wrong on a document that does not.
    pub anon_imports_end: u64,
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
    /// Declarations owlmake SYNTHESISED at read time rather than ones the source
    /// document states, keyed `kind\0IRI` (`class`, `op`, `ap`, …).
    ///
    /// Only the OBO reader fills this. OBO has no declaration syntax, so
    /// `declare_referenced_entities` invents one for every referenced entity: those
    /// are writer-side materialisation, not statements the source document makes.
    /// Recording which they are lets them be withdrawn once the import closure is
    /// known to declare the entity (see `withdraw_materialised_declarations`), so
    /// `filtered.owl`'s `IAO_0000231`, `RO_0002175`, `dc:title`, `foaf:homepage` and
    /// friends — used only as `property_value:` predicates, and all declared in
    /// `omo_import.owl` / `merged_import.owl` — get no stub of the form
    /// `<owl:AnnotationProperty rdf:about="…"/>`. A genuine declaration read from an
    /// OWL document is NOT in this set and is always rendered.
    pub materialised_declarations: std::collections::HashSet<String>,
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
    pub fn new() -> Self {
        Model {
            ont: SetOntology::new(),
            prefixes: default_prefixes(),
            build: Build::new(),
            banner_labels: std::collections::HashMap::new(),
            shared_anon: std::collections::HashMap::new(),
            rdf_shared_anon: std::collections::HashMap::new(),
            inlined_imports: Vec::new(),
            imported_components: Default::default(),
            rdf_blank_node_identity: false,
            owl_shared_owners: std::collections::HashMap::new(),
            import_order: Vec::new(),
            idspaces: Vec::new(),
            explicit_prefixes: Vec::new(),
            rdf_prefixes: Vec::new(),
            built_prefixes: Vec::new(),
            owl_genid_refs: std::collections::HashMap::new(),
            owl_label_order: std::collections::HashMap::new(),
            closure_ann_ns: Vec::new(),
            closure_declared: std::collections::HashSet::new(),
            owl_anon_blocks: Vec::new(),
            anon_alloc_base: 0,
            anon_alloc_total: 0,
            anon_hash_capacity: 0,
            anon_doc_order: Vec::new(),
            anon_imports_end: 0,
            plain_literals_typed: false,
            owlapi_456: false,
            materialised_declarations: std::collections::HashSet::new(),
            span_shared: std::collections::HashMap::new(),
            cross_shared: std::collections::HashMap::new(),
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
            shared_anon: std::collections::HashMap::new(),
            rdf_shared_anon: std::collections::HashMap::new(),
            inlined_imports: Vec::new(),
            imported_components: Default::default(),
            rdf_blank_node_identity: false,
            owl_shared_owners: std::collections::HashMap::new(),
            import_order: Vec::new(),
            idspaces: Vec::new(),
            explicit_prefixes: Vec::new(),
            rdf_prefixes: Vec::new(),
            built_prefixes: Vec::new(),
            owl_genid_refs: std::collections::HashMap::new(),
            owl_label_order: std::collections::HashMap::new(),
            closure_ann_ns: Vec::new(),
            closure_declared: std::collections::HashSet::new(),
            owl_anon_blocks: Vec::new(),
            anon_alloc_base: 0,
            anon_alloc_total: 0,
            anon_hash_capacity: 0,
            anon_doc_order: Vec::new(),
            anon_imports_end: 0,
            plain_literals_typed: false,
            owlapi_456: false,
            materialised_declarations: std::collections::HashSet::new(),
            span_shared: std::collections::HashMap::new(),
            cross_shared: std::collections::HashMap::new(),
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
        self.shared_anon = other.shared_anon.clone();
        self.rdf_shared_anon = other.rdf_shared_anon.clone();
        self.inlined_imports = other.inlined_imports.clone();
        self.imported_components = other.imported_components.clone();
        self.rdf_blank_node_identity = other.rdf_blank_node_identity;
        self.owl_shared_owners = other.owl_shared_owners.clone();
        self.import_order = other.import_order.clone();
        self.idspaces = other.idspaces.clone();
        self.rdf_prefixes = other.rdf_prefixes.clone();
        self.built_prefixes = other.built_prefixes.clone();
        self.explicit_prefixes = other.explicit_prefixes.clone();
        self.owl_genid_refs = other.owl_genid_refs.clone();
        self.owl_label_order = other.owl_label_order.clone();
        self.closure_ann_ns = other.closure_ann_ns.clone();
        self.closure_declared = other.closure_declared.clone();
        self.owl_anon_blocks = other.owl_anon_blocks.clone();
        self.anon_alloc_base = other.anon_alloc_base;
        self.anon_alloc_total = other.anon_alloc_total;
        self.anon_hash_capacity = other.anon_hash_capacity;
        self.anon_doc_order = other.anon_doc_order.clone();
        self.anon_imports_end = other.anon_imports_end;
        self.plain_literals_typed = other.plain_literals_typed;
        self.owlapi_456 = other.owlapi_456;
        self.materialised_declarations = other.materialised_declarations.clone();
        self.span_shared = other.span_shared.clone();
        self.cross_shared = other.cross_shared.clone();
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
        // The closure's declarations and annotation-property namespaces
        // described a document that still imported; once the closure's axioms
        // are the document's own, an entity the closure declared is declared
        // HERE, and suppressing its stub or its annotations hides content the
        // document now carries.
        self.closure_declared.clear();
        self.closure_ann_ns.clear();
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
        m.shared_anon = self.shared_anon.clone();
        m.rdf_shared_anon = self.rdf_shared_anon.clone();
        m.inlined_imports = self.inlined_imports.clone();
        m.imported_components = self.imported_components.clone();
        m.owl_shared_owners = self.owl_shared_owners.clone();
        m.import_order = self.import_order.clone();
        m.idspaces = self.idspaces.clone();
        m.rdf_prefixes = self.rdf_prefixes.clone();
        m.built_prefixes = self.built_prefixes.clone();
        m.explicit_prefixes = self.explicit_prefixes.clone();
        m.owl_genid_refs = self.owl_genid_refs.clone();
        m.owl_label_order = self.owl_label_order.clone();
        m.closure_ann_ns = self.closure_ann_ns.clone();
        m.closure_declared = self.closure_declared.clone();
        m.owl_anon_blocks = self.owl_anon_blocks.clone();
        m.anon_alloc_base = self.anon_alloc_base;
        m.anon_alloc_total = self.anon_alloc_total;
        m.anon_hash_capacity = self.anon_hash_capacity;
        m.anon_doc_order = self.anon_doc_order.clone();
        m.anon_imports_end = self.anon_imports_end;
        m.plain_literals_typed = self.plain_literals_typed;
        m.owlapi_456 = self.owlapi_456;
        m.materialised_declarations = self.materialised_declarations.clone();
        m.span_shared = self.span_shared.clone();
        m.cross_shared = self.cross_shared.clone();
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
    // OBO writer's `idspace:` table and the RDF/XML xmlns block need. Template
    // CURIEs expand against a separate context map that binds `dc` to dc/TERMS/
    // instead — see `template::robot_context_prefixes`.
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
