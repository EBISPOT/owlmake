# substitution

[http://purl.obolibrary.org/obo/ex/patterns/substitution.yaml](http://purl.obolibrary.org/obo/ex/patterns/substitution.yaml)

## Description

*No description*




## Variables

| Variable name | Allowed type |
|:--------------|:-------------|
| `{part}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{parts}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{code}` | xsd:string |
| `{codes}` | xsd:string |

## Name

"`{part}` `{code}` `{code}`"^^[string](http://www.w3.org/2001/XMLSchema#string)

## Annotations

- [comment](http://www.w3.org/2000/01/rdf-schema#comment): "`{part}` http://purl.obolibrary.org/obo/ex/patterns/substitution.yaml"^^[string](http://www.w3.org/2001/XMLSchema#string)
- [hasExactSynonym](http://www.geneontology.org/formats/oboInOwl#hasExactSynonym): "``"^^[string](http://www.w3.org/2001/XMLSchema#string)
- [hasNarrowSynonym](http://www.geneontology.org/formats/oboInOwl#hasNarrowSynonym): "`{codes}`"^^[string](http://www.w3.org/2001/XMLSchema#string)

## Definition



## Equivalent to



## Subclass of

[BFO_0000051](http://purl.obolibrary.org/obo/BFO_0000051) some `{part}`




## Data preview

*See full table [here](http://example.org/substitution.tsv)*

| code | codes | part_label | part | defined_class | parts |
|:--|:--|:--|:--|:--|:--|
| w2 | x1|y1|x2 |  | [EX:0000002](http://purl.obolibrary.org/obo/EX_0000002) | [EX:0000010](http://purl.obolibrary.org/obo/EX_0000010) | [EX:0000002|EX:0000003](http://purl.obolibrary.org/obo/EX_0000002|EX:0000003) |
| q |  |  | [EX:0000003](http://purl.obolibrary.org/obo/EX_0000003) | [EX:0000011](http://purl.obolibrary.org/obo/EX_0000011) | [EX:0000004|EX:0000005](http://purl.obolibrary.org/obo/EX_0000004|EX:0000005) |
| a1 | x | cogwheel | [EX:0000004](http://purl.obolibrary.org/obo/EX_0000004) | [EX:0000012](http://purl.obolibrary.org/obo/EX_0000012) | [EX:0000004](http://purl.obolibrary.org/obo/EX_0000004) |

