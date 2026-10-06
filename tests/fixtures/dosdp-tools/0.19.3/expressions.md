# expressions

[http://purl.obolibrary.org/obo/ex/patterns/expressions.yaml](http://purl.obolibrary.org/obo/ex/patterns/expressions.yaml)

## Description

*No description*




## Variables

| Variable name | Allowed type |
|:--------------|:-------------|
| `{part}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{whole}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{size}` | xsd:integer |
| `{low}` | xsd:decimal |

## Name



## Annotations



## Definition



## Equivalent to



## Subclass of

-  inverse ([BFO_0000050](http://purl.obolibrary.org/obo/BFO_0000050)) some `{whole}`
- [EX_0000101](http://purl.obolibrary.org/obo/EX_0000101) some {"a" , "b"}
- [BFO_0000051](http://purl.obolibrary.org/obo/BFO_0000051) min 2 `{part}`
- (not ([gear](http://purl.obolibrary.org/obo/EX_0000003)))  and ([BFO_0000051](http://purl.obolibrary.org/obo/BFO_0000051) only      (`{part}` or [cog](http://purl.obolibrary.org/obo/EX_0000002)))

## Other axioms

- `{part}` EquivalentTo [BFO_0000050](http://purl.obolibrary.org/obo/BFO_0000050) some `{whole}`
- `{part}` and ([BFO_0000050](http://purl.obolibrary.org/obo/BFO_0000050) some `{whole}`) SubClassOf [gear](http://purl.obolibrary.org/obo/EX_0000003)
- [expressions.yaml](http://purl.obolibrary.org/obo/ex/patterns/expressions.yaml) DisjointWith [cog](http://purl.obolibrary.org/obo/EX_0000002) or [gear](http://purl.obolibrary.org/obo/EX_0000003)

## Data preview

*See full table [here](http://example.org/expressions.tsv)*

| size | low | whole | part | defined_class |
|:--|:--|:--|:--|:--|
| 5 | 1.5 | [EX:0000003](http://purl.obolibrary.org/obo/EX_0000003) | [EX:0000002](http://purl.obolibrary.org/obo/EX_0000002) | [EX:0000020](http://purl.obolibrary.org/obo/EX_0000020) |
| 10 | 2 | [EX:0000001](http://purl.obolibrary.org/obo/EX_0000001) | [EX:0000003](http://purl.obolibrary.org/obo/EX_0000003) | [EX:0000021](http://purl.obolibrary.org/obo/EX_0000021) |

