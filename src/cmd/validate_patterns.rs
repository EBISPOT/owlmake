//! `om validate-patterns <PATH>` / `om dosdp validate -i <DIR>` — schema-validate
//! DOSDP design patterns.
//!
//! A repo's pattern gate runs this over its `dosdp-patterns/` directory before
//! generation, so a malformed pattern fails the build instead of the release. Every
//! pattern YAML is checked against the DOSDP JSON Schema (vendored below) with the
//! `jsonschema` crate; ontologies whose classes are generated from patterns — OBA,
//! CL, UBERON — depend on that gate.
//!
//! Schema validation is what catches a broken pattern: `Pattern` has no
//! `deny_unknown_fields` and carries `#[serde(default)]` on every field, so *every*
//! YAML mapping deserializes successfully. A pattern with a misspelled key, a
//! missing `text`/`vars` pair, or an `axiom_type` outside the four legal values
//! would otherwise pass unnoticed and surface only as missing axioms in a release.
//!
//! `parse_pattern` stays deliberately permissive: generation runs *after*
//! validation, so the schema is the gate and the loader accepts what it is given.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Result};
use clap::Args as ClapArgs;

use crate::model::Model;

/// The DOSDP JSON Schema, as JSON: the specification's title, `definitions` and
/// `properties`, with `additionalProperties: false` wherever the specification sets
/// it, so an unknown key is an error rather than a silently ignored clause.
///
/// Vendored as a string rather than an `include_str!` of a sidecar file so the
/// single-binary property needs no build-time asset; refresh it when the DOSDP
/// spec moves.
const DOSDP_SCHEMA_JSON: &str = r##"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "DOSDP",
  "type": "object",
  "additionalProperties": false,
  "definitions": {
    "multi_clause_printf": {
      "required": [
        "clauses"
      ],
      "additionalProperties": false,
      "properties": {
        "sep": {
          "type": "string",
          "description": "A string used as clause separator while aggregating multiple clauses.\n"
        },
        "clauses": {
          "type": "array",
          "description": "List of optional clauses.  Rules for optional clauses:  list_vars may be passed, but only one list_var per multi-clause printf is permitted. If an empty list_var is passed, the clause, and any subclauses, are omitted. If list_var with length n is passed, the clause is repeated n times, using the specified separtor to join clauses.  There is no effect on the number of subclauses in this case.\n",
          "items": {
            "$ref": "#/definitions/printf_clause"
          }
        }
      }
    },
    "printf_clause": {
      "required": [
        "text",
        "vars"
      ],
      "additionalProperties": false,
      "properties": {
        "text": {
          "description": "A print format string.",
          "type": "string"
        },
        "vars": {
          "description": "An ordered list of variables for substitution into the accompanying print format string. Each entry must correspond to the name of a variable specified in either the 'vars', 'internal_vars' or the data_var field of the pattern. Where an OWL entity is specified, the label for the OWL entity should be used in the substitution. SPECIAL RULES FOR multi_clause_printf context: In this context, list_vars are permitted. If an list is empty, the clause and any subclauses must not be added.  for lists of length > 1, mutiple clauses should be added, using the specified separator\n",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "sub_clauses": {
          "description": "List of clauses that depends on this clause. If parent clause cannot be printed, all sub_clauses are also discarded.\n",
          "type": "array",
          "items": {
            "$ref": "#/definitions/multi_clause_printf"
          }
        }
      }
    },
    "function": {
      "oneOf": [
        {
          "$ref": "#/definitions/join"
        },
        {
          "$ref": "#/definitions/regex_sub"
        }
      ]
    },
    "join": {
      "properties": {
        "sep": {
          "type": "string",
          "description": "A string used as value separator while joining list type (multi value) variables.\n"
        }
      }
    },
    "printf_annotation": {
      "type": "object",
      "additionalProperties": false,
      "oneOf": [
        {
          "required": [
            "annotationProperty",
            "text"
          ]
        },
        {
          "required": [
            "multi_clause"
          ]
        }
      ],
      "properties": {
        "annotationProperty": {
          "description": "A string corresponding to the rdfs:label of an owl annotation property. If the annotation property has no label, the shortForm ID should be used. The annotation property must be listed in the annotation property dictionary.'\n",
          "type": "string"
        },
        "annotations": {
          "items": {
            "$ref": "#/definitions/annotations"
          },
          "type": "array"
        },
        "text": {
          "description": "A print format string.",
          "type": "string"
        },
        "vars": {
          "description": "An ordered list of variables for substitution into the accompanying print format string. Each entry must correspond to the name of a variable specified in either the 'vars' field or the data_var field of the pattern. Where an OWL entity is specified, the label for the OWL entity should be used in the substitution.  An empty var list can be specified simply by leaving this field out.\n",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "multi_clause": {
          "items": {
            "$ref": "#/definitions/multi_clause_printf"
          }
        }
      }
    },
    "list_annotation": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "annotationProperty",
        "value"
      ],
      "properties": {
        "annotationProperty": {
          "description": "A string corresponding to the rdfs:label of an owl annotation property. If the annotation property has no label, the shortForm ID should be used. The annotation property must be listed in the annotation property dictionary.'\n",
          "type": "string"
        },
        "value": {
          "description": "A single list variable (list_var or data_list_var).  Each item in this list should be used to generate a separate annotation axiom.\n",
          "type": "string"
        }
      }
    },
    "iri_value_annotation": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "annotationProperty",
        "var"
      ],
      "properties": {
        "annotationProperty": {
          "description": "A string corresponding to a key in the annotation property dictionary.",
          "type": "string"
        },
        "var": {
          "description": "The name of a variable specified in the 'vars' field. The IRI of the variable value will be the object of the annotation axiom.",
          "type": "string"
        },
        "annotations": {
          "items": {
            "$ref": "#/definitions/annotations"
          },
          "type": "array"
        }
      }
    },
    "annotations": {
      "oneOf": [
        {
          "$ref": "#/definitions/printf_annotation"
        },
        {
          "$ref": "#/definitions/list_annotation"
        },
        {
          "$ref": "#/definitions/iri_value_annotation"
        }
      ]
    },
    "printf_owl": {
      "type": "object",
      "additionalProperties": false,
      "oneOf": [
        {
          "required": [
            "axiom_type",
            "text",
            "vars"
          ]
        },
        {
          "required": [
            "multi_clause"
          ]
        }
      ],
      "properties": {
        "annotations": {
          "items": {
            "$ref": "#/definitions/annotations"
          },
          "type": "array"
        },
        "axiom_type": {
          "description": "OWL axiom type expressed as manchester syntax: equivalentTo, subClassOf, disjointWith. GCI  - for general class inclusion axioms, is also valid (although missing from manchester syntax.) This specifies the axiom type to be generated from the text following substitution.'\n",
          "enum": [
            "equivalentTo",
            "subClassOf",
            "disjointWith",
            "GCI"
          ],
          "type": "string"
        },
        "text": {
          "type": "string",
          "description": "A print format string in OWL Manchester syntax. Each entry must correspond to an entry in o the name of a var in the var field of the pattern. Entries in single quotes must correspond to the labels of entries in owl_entity dictionaries (classes, relations, dataProperties)\n"
        },
        "vars": {
          "description": "An ordered list of variables for substitution into the accompanying print format string. Each entry must correspond to the name of a variable specified in either the 'vars' field or the data_var field of the pattern. An empty var list can be specified simply by leaving this field out.\n",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "multi_clause": {
          "items": {
            "$ref": "#/definitions/multi_clause_printf"
          }
        }
      }
    },
    "printf_owl_convenience": {
      "type": "object",
      "additionalProperties": false,
      "oneOf": [
        {
          "required": [
            "text",
            "vars"
          ]
        },
        {
          "required": [
            "multi_clause"
          ]
        }
      ],
      "properties": {
        "annotations": {
          "items": {
            "$ref": "#/definitions/annotations"
          },
          "type": "array"
        },
        "text": {
          "type": "string",
          "description": "A print format string in OWL Manchester syntax. Each entry must correspond to an entry in o the name of a var in the var field of the pattern. Entries in single quotes must correspond to the labels of entries in owl_entity dictionaries (classes, relations, dataProperties)\n"
        },
        "vars": {
          "description": "An ordered list of variables for substitution into the accompanying print format string. Each entry must correspond to the name of a variable\n specified in either the 'vars' field or the data_var field of the pattern.\n",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "multi_clause": {
          "items": {
            "$ref": "#/definitions/multi_clause_printf"
          }
        }
      }
    },
    "regex_sub": {
      "additionalProperties": false,
      "required": [
        "in",
        "out",
        "match",
        "sub"
      ],
      "type": "object",
      "properties": {
        "in": {
          "type": "string",
          "description": "name of input var"
        },
        "out": {
          "type": "string",
          "description": "Name of output var.  If input var specified an OWL entity then readable identifier is used as input to substitution\n"
        },
        "match": {
          "type": "string",
          "description": "perl style regex match"
        },
        "sub": {
          "type": "string",
          "description": "perl style regex sub.  May include backreferences."
        }
      }
    },
    "opa": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "edge"
      ],
      "properties": {
        "edge": {
          "description": "A triple specified as an ordered array with 3 elements [subject, rel, object] * rel must be the quoted name of a relation from the relations (object property) dictionary. * subject and object must be the name of an individual specified in the nodes field.\n",
          "type": "array",
          "items": {
            "type": "string"
          },
          "minItems": 3,
          "maxItems": 3
        },
        "annotations": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/annotations"
          }
        },
        "not": {
          "description": "Optional field for negated OPAs",
          "type": "boolean"
        }
      }
    },
    "printf_annotation_obo": {
      "type": "object",
      "additionalProperties": false,
      "oneOf": [
        {
          "required": [
            "text",
            "vars"
          ]
        },
        {
          "required": [
            "multi_clause"
          ]
        }
      ],
      "properties": {
        "annotations": {
          "items": {
            "$ref": "#/definitions/annotations"
          },
          "type": "array"
        },
        "xrefs": {
          "description": "Takes the name of a single data_list_var specifying a list of database cross references.\n",
          "type": "string",
          "mapping": "oboInOwl:hasDbXref"
        },
        "text": {
          "description": "A print format string.",
          "type": "string"
        },
        "vars": {
          "description": "An ordered list of variables for substitution into the accompanying print format string. Each entry must correspond to the name of a variable specified in either the 'vars' field or the data_var field of the pattern. Where an OWL entity is specified, the label for the OWL entity should be used in the substitution.\n",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "multi_clause": {
          "items": {
            "$ref": "#/definitions/multi_clause_printf"
          }
        }
      }
    },
    "list_annotation_obo": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "value"
      ],
      "properties": {
        "value": {
          "description": "A single list variable (list_var or data_list_var).  Each item in this list should be used to generate a separate annotation axiom.\n",
          "type": "string"
        },
        "xrefs": {
          "description": "Takes the name of a single data_list_var specifying a list of database cross references. Use of this field should add the same xref set to all annotation axioms generated.\n",
          "type": "string",
          "mapping": "oboInOwl:hasDbXref"
        }
      }
    }
  },
  "properties": {
    "pattern_name": {
      "type": "string",
      "description": "The name of the pattern.  This must be an ASCII string with no spaces. The only special characters allowed are '_' and '-'. By convention, this is used as the file name of the pattern - with an appropriate extension.\n",
      "doc_type": "root"
    },
    "pattern_iri": {
      "type": "string",
      "description": "A global identifier for the pattern. This can be a full IRI or a CURIE, using the same prefix mappings as other CURIEs in the pattern.\n",
      "doc_type": "root"
    },
    "base_IRI": {
      "type": "string",
      "description": "Specifies the base IRI to be used to generate new classes.",
      "doc_type": "root"
    },
    "contributors": {
      "type": "array",
      "items": {
        "type": "string"
      },
      "description": "A list of authors of a pattern. Each author must be specified using a URL or Curie - we recommend ORCID. We do not recommend that this list is instantiated in terms generated using a pattern, but where it is it should be instantiated as a set of annotation axioms using dc:contributor.\n",
      "doc_type": "root"
    },
    "description": {
      "type": "string",
      "description": "A free text description of the pattern.  Must be UTF-8 encoded.",
      "doc_type": "root"
    },
    "examples": {
      "type": "array",
      "items": {
        "type": "string"
      },
      "description": "A list of example terms implementing this pattern.",
      "doc_type": "root"
    },
    "status": {
      "type": "string",
      "description": "Implementation status of pattern.",
      "enum": [
        "development",
        "published"
      ],
      "doc_type": "root"
    },
    "tags": {
      "type": "array",
      "items": {
        "type": "string"
      },
      "description": "A list of strings used to tag a pattern for the purposes of arbitrary, cross-cutting grouping of patterns.\n",
      "doc_type": "root"
    },
    "readable_identifiers": {
      "type": "array",
      "items": {
        "type": "string"
      },
      "description": "A list of annotation properties used as naming fields, in order of preference.",
      "doc_type": "root"
    },
    "classes": {
      "type": "object",
      "description": "A dictionary of OWL classes. key :label; value : short form id",
      "doc_type": "owl_entity_dict"
    },
    "objectProperties": {
      "type": "object",
      "description": "A dictionary of OWL object properties. key : label; value : short form id",
      "doc_type": "owl_entity_dict"
    },
    "relations": {
      "type": "object",
      "description": "A dictionary of OWL object properties. key : label; value : short form id",
      "doc_type": "owl_entity_dict"
    },
    "dataProperties": {
      "type": "object",
      "description": "A dictionary of OWL data properties key : label; value : short form id",
      "doc_type": "owl_entity_dict"
    },
    "annotationProperties": {
      "type": "object",
      "description": "A dictionary of OWL annotation properties key : label; value : short form id",
      "doc_type": "owl_entity_dict"
    },
    "vars": {
      "type": "object",
      "propertyNames": {
        "pattern": "^[A-Za-z_][A-Za-z0-9_]*$"
      },
      "description": "A dictionary of variables ranging over OWL classes. Key = variable name, value = variable range as manchester syntax string.\n",
      "doc_type": "var_types"
    },
    "list_vars": {
      "type": "object",
      "propertyNames": {
        "pattern": "^[A-Za-z_][A-Za-z0-9_]*$"
      },
      "description": "A dictionary of variables refering to lists of owl classes. Key = variable name, value = variable range of items in list specified as a valid OWL data-type.\n",
      "doc_type": "var_types"
    },
    "data_vars": {
      "type": "object",
      "propertyNames": {
        "pattern": "^[A-Za-z_][A-Za-z0-9_]*$"
      },
      "description": "A dictionary of variables ranging over OWL data-types. Key = variable name, value = variable range specified as a valid OWL data-type.\n",
      "doc_type": "var_types"
    },
    "data_list_vars": {
      "type": "object",
      "propertyNames": {
        "pattern": "^[A-Za-z_][A-Za-z0-9_]*$"
      },
      "description": "A dictionary of variables rrefering to lists of some specified OWL data-types. Key = variable name, value = variable range of all items in list, specified as a valid OWL data-type.\n",
      "doc_type": "var_types"
    },
    "internal_vars": {
      "type": "array",
      "properties": {
        "var_name": {
          "pattern": "^[A-Za-z_][A-Za-z0-9_]*$",
          "description": "Name of the internal variable to be defined. Expected naming pattern is ^[A-Za-z_][A-Za-z0-9_]*$\n",
          "type": "string"
        },
        "input": {
          "description": "A list_vars or data_list_vars variable to which the given function applied.\n",
          "type": "string"
        },
        "apply": {
          "$ref": "#/definitions/function"
        }
      },
      "description": "List of internal variable construction definitions. Given function is applied to the given multi value input and the result is defined as a new internal variable.\n",
      "doc_type": "var_types"
    },
    "substitutions": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/regex_sub"
      },
      "doc_type": "var_munging"
    },
    "annotations": {
      "items": {
        "$ref": "#/definitions/annotations"
      },
      "type": "array",
      "doc_type": "axioms"
    },
    "logical_axioms": {
      "items": {
        "$ref": "#/definitions/printf_owl"
      },
      "type": "array",
      "doc_type": "axioms"
    },
    "equivalentTo": {
      "$ref": "#/definitions/printf_owl_convenience",
      "doc_type": "convenience"
    },
    "subClassOf": {
      "$ref": "#/definitions/printf_owl_convenience",
      "doc_type": "convenience"
    },
    "GCI": {
      "$ref": "#/definitions/printf_owl_convenience",
      "doc_type": "convenience"
    },
    "disjointWith": {
      "$ref": "#/definitions/printf_owl_convenience",
      "doc_type": "convenience"
    },
    "name": {
      "$ref": "#/definitions/printf_annotation_obo",
      "mapping": "rdfs:label",
      "doc_type": "obo"
    },
    "comment": {
      "$ref": "#/definitions/printf_annotation_obo",
      "mapping": "rdfs:comment",
      "doc_type": "obo"
    },
    "def": {
      "$ref": "#/definitions/printf_annotation_obo",
      "mapping": "obo:IAO_0000115",
      "doc_type": "obo"
    },
    "namespace": {
      "$ref": "#/definitions/printf_annotation_obo",
      "mapping": "oboInOwl:hasOBONamespace",
      "doc_type": "obo"
    },
    "exact_synonym": {
      "$ref": "#/definitions/list_annotation_obo",
      "mapping": "oboInOwl:hasExactSynonym",
      "doc_type": "obo"
    },
    "narrow_synonym": {
      "$ref": "#/definitions/list_annotation_obo",
      "mapping": "oboInOwl:hasNarrowSynonym",
      "doc_type": "obo"
    },
    "related_synonym": {
      "$ref": "#/definitions/list_annotation_obo",
      "mapping": "oboInOwl:hasRelatedSynonym",
      "doc_type": "obo"
    },
    "broad_synonym": {
      "$ref": "#/definitions/list_annotation_obo",
      "mapping": "oboInOwl:hasBroadSynonym",
      "doc_type": "obo"
    },
    "xref": {
      "$ref": "#/definitions/list_annotation_obo",
      "mapping": "oboInOwl:hasDbXref",
      "doc_type": "obo"
    },
    "generated_synonyms": {
      "description": "An OBO convenience field to allow the specification of exact synonyms generated by interpolation of OWL entity names into printf text. Each entry may be annotated.\n",
      "type": "array",
      "items": {
        "$ref": "#/definitions/printf_annotation_obo",
        "mapping": "oboInOwl:hasExactSynonym"
      },
      "doc_type": "obo"
    },
    "generated_narrow_synonyms": {
      "description": "An OBO convenience field to allow the specification of narrow synonyms generated by interpolation of OWL entity names into printf text. Each entry may be annotated.\n",
      "type": "array",
      "items": {
        "$ref": "#/definitions/printf_annotation_obo",
        "mapping": "oboInOwl:hasNarrowSynonym"
      },
      "doc_type": "obo"
    },
    "generated_broad_synonyms": {
      "description": "An OBO convenience field to allow the specification of broad synonyms generated by interpolation of OWL entity names into printf text. Each entry may be annotated.\n",
      "type": "array",
      "items": {
        "$ref": "#/definitions/printf_annotation_obo",
        "mapping": "oboInOwl:hasBroadSynonym"
      },
      "doc_type": "obo"
    },
    "generated_related_synonyms": {
      "description": "An OBO convenience field to allow the specification of related synonyms generated by interpolation of OWL entity names into printf text. Each entry may be annotated.\n",
      "type": "array",
      "items": {
        "$ref": "#/definitions/printf_annotation_obo",
        "mapping": "oboInOwl:hasBroadSynonym"
      },
      "doc_type": "obo"
    },
    "instance_graph": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "nodes",
        "edges"
      ],
      "properties": {
        "nodes": {
          "description": "Key = name of individual within this pattern doc Value = Type of individual specified using either the quoted name of a class in the class dictionary of this pattern or a var name.  This field does not support typing via anonymous class expressions\n",
          "type": "object"
        },
        "edges": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/opa"
          }
        }
      },
      "doc_type": "instance_graph"
    }
  }
}"##;

#[derive(ClapArgs)]
pub struct Args {
    /// Pattern file(s), or a directory of them (`*.yaml`/`*.yml`).
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,

    /// The same, spelled as `dosdp validate` spells it (`-i <DIR>`).
    #[arg(short = 'i', long = "input", value_name = "PATH")]
    pub input: Vec<PathBuf>,
}

/// A side-output command: it reports on files and never touches the in-flight
/// ontology, so a chained model passes straight through.
pub fn step(model: Option<Model>, a: &Args) -> Result<Option<Model>> {
    run(a)?;
    Ok(model)
}

pub fn run(a: &Args) -> Result<()> {
    let roots: Vec<PathBuf> = a.input.iter().chain(a.paths.iter()).cloned().collect();
    if roots.is_empty() {
        bail!("validate-patterns: no pattern file or directory given");
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
        files.extend(collect(root)?);
    }
    if files.is_empty() {
        // A pattern directory with nothing in it is a legitimate state (a repo
        // that declares patterns but ships none yet), not a validation failure.
        status!("validate-patterns: no pattern YAMLs found in {}", render_list(&roots));
        return Ok(());
    }

    let mut failed = 0usize;
    for f in &files {
        match validate_file(f) {
            Ok(()) => {}
            Err(e) => {
                failed += 1;
                eprintln!("{e:#}");
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} DOSDP pattern(s) failed schema validation", files.len());
    }
    status!("validate-patterns: {} pattern(s) OK", files.len());
    Ok(())
}

/// Pattern YAMLs under `root`: the file itself, or the `*.yaml`/`*.yml` in a
/// directory (sorted, non-recursive — `dosdp-patterns/` is flat, and a recursive
/// walk would pick up the `data/` tables that live beside it).
fn collect(root: &Path) -> Result<Vec<PathBuf>> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        bail!("validate-patterns: no such file or directory: {}", root.display());
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| anyhow!("validate-patterns: reading {}: {e}", root.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x == "yaml" || x == "yml")
                    .unwrap_or(false)
        })
        .collect();
    out.sort();
    Ok(out)
}

fn render_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
}

/// The compiled DOSDP schema, built once per process.
fn schema() -> &'static jsonschema::JSONSchema {
    static COMPILED: OnceLock<jsonschema::JSONSchema> = OnceLock::new();
    COMPILED.get_or_init(|| {
        let value: serde_json::Value =
            serde_json::from_str(DOSDP_SCHEMA_JSON).expect("internal: vendored DOSDP schema is not JSON");
        jsonschema::JSONSchema::compile(&value)
            .expect("internal: vendored DOSDP schema is not a valid JSON Schema")
    })
}

/// Validate one pattern file against the DOSDP schema.
pub fn validate_file(path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("validate-patterns: reading {}: {e}", path.display()))?;
    validate_text(&text).map_err(|e| anyhow!("{}: {e}", path.display()))
}

/// Validate one pattern document. Reported errors carry the instance path
/// (`/logical_axioms/0/axiom_type`) so a curator can find the offending key.
pub fn validate_text(yaml: &str) -> Result<()> {
    // A leading BOM defeats every YAML parser; `parse_pattern` strips one too.
    let yaml = yaml.strip_prefix('\u{feff}').unwrap_or(yaml);
    // YAML → JSON value tree: the schema is JSON Schema, and YAML is a superset
    // of JSON, so a pattern validates in exactly the shape the schema describes.
    let value: serde_json::Value = serde_yaml::from_str(yaml)
        .map_err(|e| anyhow!("not valid YAML: {e}"))?;
    if !value.is_object() {
        bail!("not a DOSDP pattern (the document is not a mapping)");
    }
    if let Err(errors) = schema().validate(&value) {
        let mut msgs: Vec<String> = errors
            .map(|e| {
                let path = e.instance_path.to_string();
                let at = if path.is_empty() { "(root)".to_string() } else { path };
                format!("  at {at}: {e}")
            })
            .collect();
        msgs.sort();
        msgs.dedup();
        bail!("does not conform to the DOSDP schema:\n{}", msgs.join("\n"));
    }
    Ok(())
}

/// Entry point for `om dosdp validate …` and the `dosdp` PATH shim, invoked as
/// `dosdp validate -i <DIR>`.
pub fn validate_main(args: &[String]) -> i32 {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "dosdp validate (owlmake {}) — check DOSDP patterns against the DOSDP schema\n\n\
                     Usage: dosdp validate -i <FILE-OR-DIR>...",
                    env!("CARGO_PKG_VERSION")
                );
                return 0;
            }
            "-i" | "--input" => {
                i += 1;
                match args.get(i) {
                    Some(p) => paths.push(PathBuf::from(p)),
                    None => {
                        eprintln!("dosdp validate: -i expects a path");
                        return 1;
                    }
                }
            }
            t if t.starts_with("--input=") => paths.push(PathBuf::from(&t["--input=".len()..])),
            t if t.starts_with('-') => {
                eprintln!("dosdp validate: unrecognised option `{t}`");
                return 1;
            }
            t => paths.push(PathBuf::from(t)),
        }
        i += 1;
    }
    match run(&Args { paths, input: Vec::new() }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("dosdp validate: {e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal pattern that conforms to the DOSDP schema.
    const GOOD: &str = r#"
pattern_name: entity_attribute
pattern_iri: http://purl.obolibrary.org/obo/oba/entity_attribute.yaml
description: "An attribute of an entity."
contributors:
  - https://orcid.org/0000-0002-6601-2165
classes:
  quality: PATO:0000001
  entity: BFO:0000001
relations:
  characteristic_of: RO:0000052
vars:
  attribute: "'quality'"
  entity: "'entity'"
name:
  text: "%s of %s"
  vars:
    - attribute
    - entity
def:
  text: "A %s which inheres in a %s."
  vars:
    - attribute
    - entity
equivalentTo:
  text: "'quality' that 'characteristic_of' some %s"
  vars:
    - entity
"#;

    #[test]
    fn the_vendored_schema_compiles() {
        // Panics inside `schema()` if the vendored JSON is malformed.
        let _ = schema();
    }

    #[test]
    fn a_well_formed_pattern_passes() {
        validate_text(GOOD).unwrap();
    }

    /// The case a schema check has to catch: `Pattern` has no
    /// `deny_unknown_fields`, so serde alone swallows a misspelled key and the
    /// pattern deserializes as if the clause were simply absent.
    #[test]
    fn a_misspelled_key_is_rejected() {
        let bad = GOOD.replace("pattern_name:", "patternName:");
        let e = validate_text(&bad).unwrap_err().to_string();
        assert!(e.contains("does not conform"), "{e}");
        assert!(e.contains("patternName"), "{e}");
    }

    /// `printf_owl_convenience` requires `text` AND `vars` (or a `multi_clause`);
    /// serde fills both with defaults and never complains, so only the schema
    /// rejects a clause that names neither.
    #[test]
    fn a_logical_template_missing_its_vars_is_rejected() {
        let bad = GOOD.replace("equivalentTo:\n  text:", "equivalentTo:\n  txet:");
        assert!(validate_text(&bad).is_err());
    }

    /// `axiom_type` is an enum of four values in the schema; anything else is a
    /// pattern that generates nothing.
    #[test]
    fn an_unknown_axiom_type_is_rejected() {
        let bad = format!(
            "{GOOD}logical_axioms:\n  - axiom_type: subclassOf\n    text: \"%s\"\n    vars:\n      - entity\n"
        );
        let e = validate_text(&bad).unwrap_err().to_string();
        assert!(e.contains("axiom_type"), "{e}");
        // The correct spelling passes, so the enum is being read, not the key.
        let ok = bad.replace("subclassOf", "subClassOf");
        validate_text(&ok).unwrap();
    }

    #[test]
    fn a_non_mapping_document_is_rejected() {
        assert!(validate_text("- a\n- b\n").is_err());
        assert!(validate_text("").is_err());
    }

    #[test]
    fn a_directory_of_patterns_is_globbed() {
        let dir = std::env::temp_dir().join(format!("owlmake-patterns-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.yaml"), GOOD).unwrap();
        std::fs::write(dir.join("b.yml"), GOOD).unwrap();
        // Not a pattern: the data table that lives beside the patterns.
        std::fs::write(dir.join("a.tsv"), "defined_class\tentity\n").unwrap();
        let found = collect(&dir).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(run(&Args { paths: vec![dir.clone()], input: Vec::new() }).is_ok());

        // One broken pattern fails the whole directory: the gate is all-or-nothing.
        std::fs::write(dir.join("c.yaml"), GOOD.replace("pattern_name:", "patternName:")).unwrap();
        assert!(run(&Args { paths: Vec::new(), input: vec![dir.clone()] }).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The invocation shape the `dosdp` PATH shim accepts: `dosdp validate -i <DIR>`.
    #[test]
    fn the_odk_invocation_is_accepted() {
        let dir = std::env::temp_dir().join(format!("owlmake-patterns-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.yaml"), GOOD).unwrap();
        assert_eq!(
            validate_main(&["-i".to_string(), dir.display().to_string()]),
            0
        );
        std::fs::write(dir.join("bad.yaml"), "pattern_name: [1, 2]\n").unwrap();
        assert_eq!(
            validate_main(&["-i".to_string(), dir.display().to_string()]),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
