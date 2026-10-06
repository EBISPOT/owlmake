# list_xrefs

[http://purl.obolibrary.org/obo/ex/patterns/list_xrefs.yaml](http://purl.obolibrary.org/obo/ex/patterns/list_xrefs.yaml)

## Description

*No description*




## Variables

| Variable name | Allowed type |
|:--------------|:-------------|
| `{part}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{parts}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{code}` | xsd:string |
| `{ref}` | xsd:string |
| `{codes}` | xsd:string |
| `{refs}` | xsd:string |

## Name



## Annotations

- [hasExactSynonym](http://www.geneontology.org/formats/oboInOwl#hasExactSynonym): "`{codes}`"^^[string](http://www.w3.org/2001/XMLSchema#string)
- [hasBroadSynonym](http://www.geneontology.org/formats/oboInOwl#hasBroadSynonym): "`{codes}`"^^[string](http://www.w3.org/2001/XMLSchema#string)
- [comment](http://www.w3.org/2000/01/rdf-schema#comment): "c `{part}`."^^[string](http://www.w3.org/2001/XMLSchema#string)
- [comment](http://www.w3.org/2000/01/rdf-schema#comment): "`{codes}`"^^[string](http://www.w3.org/2001/XMLSchema#string)
- [hasNarrowSynonym](http://www.geneontology.org/formats/oboInOwl#hasNarrowSynonym): "`{codes}`"^^[string](http://www.w3.org/2001/XMLSchema#string)
- [hasRelatedSynonym](http://www.geneontology.org/formats/oboInOwl#hasRelatedSynonym): "`{codes}`"^^[string](http://www.w3.org/2001/XMLSchema#string)

## Definition

"A widget with `{part}`."^^[string](http://www.w3.org/2001/XMLSchema#string)

## Equivalent to



## Subclass of

[widget](http://purl.obolibrary.org/obo/EX_0000001)  and ([BFO_0000051](http://purl.obolibrary.org/obo/BFO_0000051) some `{part}`)




## Data preview

*See full table [here](http://example.org/list-xrefs.tsv)*

| undeclared | ref | codes | part | defined_class | parts | refs | code |
|:--|:--|:--|:--|:--|:--|:--|:--|
| [U:1](http://purl.obolibrary.org/obo/U_1) | [R:1](http://purl.obolibrary.org/obo/R_1) | w2a|w2b | [EX:0000002](http://purl.obolibrary.org/obo/EX_0000002) | [EX:0000010](http://purl.obolibrary.org/obo/EX_0000010) | [EX:0000002|EX:0000003](http://purl.obolibrary.org/obo/EX_0000002|EX:0000003) | [R:2|R:3](http://purl.obolibrary.org/obo/R_2|R:3) | w2 |

