# source_curie_iri

[EX:pat](EX:pat)

## Description

Test pattern.




## Variables

| Variable name | Allowed type |
|:--------------|:-------------|
| `{part}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{note}` | xsd:string |
| `{syns}` | xsd:string |
| `{refs}` | xsd:string |

## Name



## Annotations



## Definition



## Equivalent to






## Other axioms

- [EX_pat](http://purl.obolibrary.org/obo/EX_pat) [IAO_0000115](http://purl.obolibrary.org/obo/IAO_0000115) "A widget with `{part}`."^^[string](http://www.w3.org/2001/XMLSchema#string)
- [EX_pat](http://purl.obolibrary.org/obo/EX_pat) [comment](http://www.w3.org/2000/01/rdf-schema#comment) "`{note}`"^^[string](http://www.w3.org/2001/XMLSchema#string)
- [EX_pat](http://purl.obolibrary.org/obo/EX_pat) [label](http://www.w3.org/2000/01/rdf-schema#label) "widget with `{part}`"^^[string](http://www.w3.org/2001/XMLSchema#string)
- [EX_pat](http://purl.obolibrary.org/obo/EX_pat) SubClassOf [widget](http://purl.obolibrary.org/obo/EX_0000001) and ([BFO_0000051](http://purl.obolibrary.org/obo/BFO_0000051) some `{part}`)
- [EX_pat](http://purl.obolibrary.org/obo/EX_pat) [hasExactSynonym](http://www.geneontology.org/formats/oboInOwl#hasExactSynonym) "`{syns}`"^^[string](http://www.w3.org/2001/XMLSchema#string)

## Data preview

*See full table [here](http://example.org/source.tsv)*

| syns | refs | note | part | defined_class |
|:--|:--|:--|:--|:--|
| s1|s2 | [R:1|R:2](http://purl.obolibrary.org/obo/R_1|R:2) | a note | [EX:0000002](http://purl.obolibrary.org/obo/EX_0000002) | [EX:0000010](http://purl.obolibrary.org/obo/EX_0000010) |

