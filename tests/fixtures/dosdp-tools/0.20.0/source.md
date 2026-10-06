# source

[http://purl.obolibrary.org/obo/ex/patterns/source.yaml](http://purl.obolibrary.org/obo/ex/patterns/source.yaml)

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

"widget with `{part}`"^^[string](http://www.w3.org/2001/XMLSchema#string)

## Annotations

- [hasExactSynonym](http://www.geneontology.org/formats/oboInOwl#hasExactSynonym): "`{syns}`"^^[string](http://www.w3.org/2001/XMLSchema#string)
- [comment](http://www.w3.org/2000/01/rdf-schema#comment): "`{note}`"^^[string](http://www.w3.org/2001/XMLSchema#string)

## Definition

"A widget with `{part}`."^^[string](http://www.w3.org/2001/XMLSchema#string)

## Equivalent to



## Subclass of

[widget](http://purl.obolibrary.org/obo/EX_0000001)  and ([BFO_0000051](http://purl.obolibrary.org/obo/BFO_0000051) some `{part}`)




## Data preview

*See full table [here](http://example.org/source.tsv)*

| syns | refs | note | part | defined_class |
|:--|:--|:--|:--|:--|
| s1|s2 | [R:1|R:2](http://purl.obolibrary.org/obo/R_1|R:2) | a note | [EX:0000002](http://purl.obolibrary.org/obo/EX_0000002) | [EX:0000010](http://purl.obolibrary.org/obo/EX_0000010) |

