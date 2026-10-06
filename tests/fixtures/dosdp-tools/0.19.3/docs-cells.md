# docs-escape

[http://purl.obolibrary.org/obo/ex/patterns/docs-escape.yaml](http://purl.obolibrary.org/obo/ex/patterns/docs-escape.yaml)

## Description

Text with markup & letters beyond ASCII.




## Variables

| Variable name | Allowed type |
|:--------------|:-------------|
| `{part}` | [widget](http://purl.obolibrary.org/obo/EX_0000001) |
| `{code}` | xsd:string |

## Name

"&beta;-`{part}` &amp; &lt;caf&eacute;&gt; &quot;`{code}`&quot;"^^[string](http://www.w3.org/2001/XMLSchema#string)

## Annotations



## Definition

"A widget&mdash;with `{part}`&mdash;costs 5 &euro;."^^[string](http://www.w3.org/2001/XMLSchema#string)

## Equivalent to

[widget](http://purl.obolibrary.org/obo/EX_0000001)  and ([BFO_0000051](http://purl.obolibrary.org/obo/BFO_0000051) some `{part}`)  and ([EX_0000201](http://purl.obolibrary.org/obo/EX_0000201) some {"a" , "b"})  and ([EX_0000201](http://purl.obolibrary.org/obo/EX_0000201) value 5)







## Data preview

*See full table [here](http://example.org/docs-cells.csv)*

| defined_class | part | code |
|:--|:--|:--|
| [EX:0000010](http://purl.obolibrary.org/obo/EX_0000010) | [EX:0000002](http://purl.obolibrary.org/obo/EX_0000002) | line one
line two |
| [EX:0000011](http://purl.obolibrary.org/obo/EX_0000011) | [EX:0000003](http://purl.obolibrary.org/obo/EX_0000003) | a, b |

