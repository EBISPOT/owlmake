# Ontology comparison

## Left
- Ontology IRI: `http://example.org/diff-render`
- Version IRI: `http://example.org/diff-render/v1`
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/diff-render-left.ofn`

## Right
- Ontology IRI: `http://example.org/diff-imports`
- Version IRI: *None*
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/diff-imports.ofn`

### Ontology imports 

#### Added
- [diff-imports-child](http://example.org/diff-imports-child) 

### Ontology annotations 



### GO:0000002 `http://purl.obolibrary.org/obo/GO_0000002`

#### Added
- Class: [GO:0000002](http://purl.obolibrary.org/obo/GO_0000002) 

- [GO:0000002](http://purl.obolibrary.org/obo/GO_0000002) [label](http://www.w3.org/2000/01/rdf-schema#label) "GO:0000002" 

- [GO:0000002](http://purl.obolibrary.org/obo/GO_0000002) SubClassOf [line one
line two](http://example.org/diff-imports#B) 


### GO_0000001 `http://purl.obolibrary.org/obo/GO_0000001`

#### Added
- Class: [GO_0000001](http://purl.obolibrary.org/obo/GO_0000001) 

- [GO_0000001](http://purl.obolibrary.org/obo/GO_0000001) SubClassOf [imported label](http://purl.obolibrary.org/obo/UBERON_0000001) 


### Z `http://example.org/diff-render#Z`
#### Removed
- Class: [Z](http://example.org/diff-render#Z) 



### import label of A `http://example.org/diff-imports#A`

#### Added
- Class: [import label of A](http://example.org/diff-imports#A) 

- [import label of A](http://example.org/diff-imports#A) SubClassOf [from the import](http://example.org/diff-imports-child#L) 


### line one
line two `http://example.org/diff-imports#B`

#### Added
- Class: [line one
line two](http://example.org/diff-imports#B) 

- [line one
line two](http://example.org/diff-imports#B) [label](http://www.w3.org/2000/01/rdf-schema#label) "line one
line two" 

