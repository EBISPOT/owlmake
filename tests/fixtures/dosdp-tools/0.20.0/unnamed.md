# labels

[http://purl.obolibrary.org/obo/ex/patterns/labels.yaml](http://purl.obolibrary.org/obo/ex/patterns/labels.yaml)

## Description

*No description*




## Variables

| Variable name | Allowed type |
|:--------------|:-------------|
| `{part}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{other}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{parts}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{code}` | xsd:string |

## Name

"`{part}` with `{other}`"^^[string](http://www.w3.org/2001/XMLSchema#string)

## Annotations

- [hasExactSynonym](http://www.geneontology.org/formats/oboInOwl#hasExactSynonym): "`{parts}`"^^[string](http://www.w3.org/2001/XMLSchema#string)

## Definition

"A widget with `{part}`, coded `{code}`."^^[string](http://www.w3.org/2001/XMLSchema#string)

## Equivalent to



## Subclass of

[BFO_0000051](http://purl.obolibrary.org/obo/BFO_0000051) some `{part}`




## Data preview

*See full table [here](http://example.org/unnamed.tsv)*

| code | part | defined_class | other | parts |
|:--|:--|:--|:--|:--|
| c1 | widget | foo | 'widget' | [widget|EX:0000002](http://purl.obolibrary.org/obo/widget|EX_0000002) |

