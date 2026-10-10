# Ontology comparison

## Left
- Ontology IRI: `http://example.org/diff-render`
- Version IRI: `http://example.org/diff-render/v1`
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/diff-render-left.ofn`

## Right
- Ontology IRI: `http://example.org/diff-render`
- Version IRI: `http://example.org/diff-render/v2`
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/diff-render.ofn`

### Ontology imports 



### Ontology annotations 

#### Added
- [comment](http://www.w3.org/2000/01/rdf-schema#comment) "ontology" 

- [seeAlso](http://www.w3.org/2000/01/rdf-schema#seeAlso) [alpha](http://example.org/diff-render#A) 


### <http://example.org/diff-render#e> `http://example.org/diff-render#e`

#### Added
- DataProperty: [<http://example.org/diff-render#e>](http://example.org/diff-render#e) 

- [<http://example.org/diff-render#e>](http://example.org/diff-render#e) [label](http://www.w3.org/2000/01/rdf-schema#label) "&lt;http://example.org/diff-render#e&gt;" 


### B `http://example.org/diff-render#q`

#### Added
- ObjectProperty: [B](http://example.org/diff-render#q) 

- [B](http://example.org/diff-render#q) [label](http://www.w3.org/2000/01/rdf-schema#label) [B](http://example.org/diff-render#B) 


### B `http://example.org/diff-render#B`

#### Added
- Class: [B](http://example.org/diff-render#B) 

- [B](http://example.org/diff-render#B) SubClassOf [dee](http://example.org/diff-render#d) value 5 

- [B](http://example.org/diff-render#B) SubClassOf [dee](http://example.org/diff-render#d) value "cinq"@fr 

- [B](http://example.org/diff-render#B) SubClassOf [dee](http://example.org/diff-render#d) value "five" 

- [B](http://example.org/diff-render#B) SubClassOf [pee](http://example.org/diff-render#p) value _:genid2147483648 

- [B](http://example.org/diff-render#B) SubClassOf [pee](http://example.org/diff-render#p) some [Thing](http://www.w3.org/2002/07/owl#Thing) 


### Rules 

#### Added
- [alpha](http://example.org/diff-render#A)(?<http://example.org/diff-render#v>), [dee](http://example.org/diff-render#d)(?<http://example.org/diff-render#v>, ?<http://example.org/diff-render#w>) -> [B](http://example.org/diff-render#B)(?<http://example.org/diff-render#v>) 

- [alpha](http://example.org/diff-render#A)(?<http://example.org/diff-render#v>), [pee](http://example.org/diff-render#p)(?<http://example.org/diff-render#v>, ?<http://example.org/diff-render#w>), swrlb:greaterThan(?<http://example.org/diff-render#n>, 5) -> [B](http://example.org/diff-render#B)(?<http://example.org/diff-render#w>),  SameAs (?<http://example.org/diff-render#v>, ?<http://example.org/diff-render#w>) 


### XAO_0000_1 `http://purl.obolibrary.org/obo/XAO_0000_1`

#### Added
- Class: [XAO_0000_1](http://purl.obolibrary.org/obo/XAO_0000_1) 


### Z `http://example.org/diff-render#Z`
#### Removed
- Class: [Z](http://example.org/diff-render#Z) 



### _:genid2147483648 

#### Added
- _:genid2147483648 [a p](http://example.org/diff-render#ap) "anon" 


### a p `http://example.org/diff-render#ap`

#### Added
- AnnotationProperty: [a p](http://example.org/diff-render#ap) 

- [a p](http://example.org/diff-render#ap) [label](http://www.w3.org/2000/01/rdf-schema#label) "a p" 

- [a p](http://example.org/diff-render#ap) Domain [alpha](http://example.org/diff-render#A) 

- [a p](http://example.org/diff-render#ap) Range [B](http://example.org/diff-render#B) 

- [a p](http://example.org/diff-render#ap) SubPropertyOf: [aq](http://example.org/diff-render#aq) 


### alpha `http://example.org/diff-render#A`

#### Added
- Class: [alpha](http://example.org/diff-render#A) 

- [alpha](http://example.org/diff-render#A) [a p](http://example.org/diff-render#ap) [B](http://example.org/diff-render#B) 

- [alpha](http://example.org/diff-render#A) [a p](http://example.org/diff-render#ap) [no-label](http://example.org/no-label) 

- [alpha](http://example.org/diff-render#A) [a p](http://example.org/diff-render#ap) "z
two lines &quot;quoted&quot; back\slash" 
  - [comment](http://www.w3.org/2000/01/rdf-schema#comment) "nested" 

- [alpha](http://example.org/diff-render#A) [comment](http://www.w3.org/2000/01/rdf-schema#comment) 1.5 

- [alpha](http://example.org/diff-render#A) [comment](http://www.w3.org/2000/01/rdf-schema#comment) "u"^^[dt](http://example.org/diff-render#dt) 

- [alpha](http://example.org/diff-render#A) [label](http://www.w3.org/2000/01/rdf-schema#label) "alpha" 

- [alpha](http://example.org/diff-render#A) DisjointWith [B](http://example.org/diff-render#B) 

- [alpha](http://example.org/diff-render#A) DisjointUnionOf [B](http://example.org/diff-render#B), not ([B](http://example.org/diff-render#B)) 

- [alpha](http://example.org/diff-render#A) EquivalentTo [B](http://example.org/diff-render#B) 

- [alpha](http://example.org/diff-render#A) HasKey [pee](http://example.org/diff-render#p)[dee](http://example.org/diff-render#d) 

- [alpha](http://example.org/diff-render#A) SubClassOf [B](http://example.org/diff-render#B) and ([dee](http://example.org/diff-render#d) some [string](http://www.w3.org/2001/XMLSchema#string)) 
  - [comment](http://www.w3.org/2000/01/rdf-schema#comment) "c" 


### aq `http://example.org/diff-render#aq`

#### Added
- AnnotationProperty: [aq](http://example.org/diff-render#aq) 


### dee `http://example.org/diff-render#d`

#### Added
- DataProperty: [dee](http://example.org/diff-render#d) 

- [dee](http://example.org/diff-render#d) [label](http://www.w3.org/2000/01/rdf-schema#label) "dee" 

- [dee](http://example.org/diff-render#d) Domain [alpha](http://example.org/diff-render#A) 

- [dee](http://example.org/diff-render#d) Range: ([integer](http://www.w3.org/2001/XMLSchema#integer) or [string](http://www.w3.org/2001/XMLSchema#string)) 

-  Functional: [dee](http://example.org/diff-render#d) 

- [dee](http://example.org/diff-render#d) SubPropertyOf: [<http://example.org/diff-render#e>](http://example.org/diff-render#e) 


### eye `http://example.org/diff-render#i`

#### Added
- Individual: [eye](http://example.org/diff-render#i) 

- [eye](http://example.org/diff-render#i) [label](http://www.w3.org/2000/01/rdf-schema#label) "eye" 

- [eye](http://example.org/diff-render#i) Type [alpha](http://example.org/diff-render#A) 

- [eye](http://example.org/diff-render#i) [dee](http://example.org/diff-render#d) "x" 

- [eye](http://example.org/diff-render#i) DifferentFrom [j](http://example.org/diff-render#j) 

-  not ([eye](http://example.org/diff-render#i) [dee](http://example.org/diff-render#d) "y") 

-  not ([eye](http://example.org/diff-render#i) [pee](http://example.org/diff-render#p) [j](http://example.org/diff-render#j)) 

- [eye](http://example.org/diff-render#i) [pee](http://example.org/diff-render#p) [j](http://example.org/diff-render#j) 

-  SameIndividual: [eye](http://example.org/diff-render#i), [j](http://example.org/diff-render#j), [k](http://example.org/diff-render#k) 

- [eye](http://example.org/diff-render#i) SameAs [j](http://example.org/diff-render#j) 


### go one `http://purl.obolibrary.org/obo/GO_0000001`

#### Added
- Class: [go one](http://purl.obolibrary.org/obo/GO_0000001) 

- [go one](http://purl.obolibrary.org/obo/GO_0000001) [label](http://www.w3.org/2000/01/rdf-schema#label) "go one" 

- [go one](http://purl.obolibrary.org/obo/GO_0000001) SubClassOf [XAO_0000_1](http://purl.obolibrary.org/obo/XAO_0000_1) 


### integer[>= 1] 

#### Added
-  


### j `http://example.org/diff-render#j`

#### Added
- Individual: [j](http://example.org/diff-render#j) 


### k `http://example.org/diff-render#k`

#### Added
- Individual: [k](http://example.org/diff-render#k) 

- [k](http://example.org/diff-render#k) SameAs _:genid2147483649 


### pee `http://example.org/diff-render#p`

#### Added
- ObjectProperty: [pee](http://example.org/diff-render#p) 

- [pee](http://example.org/diff-render#p) [label](http://www.w3.org/2000/01/rdf-schema#label) "pee" 

- [pee](http://example.org/diff-render#p) DisjointWith  inverse ([B](http://example.org/diff-render#q)) 

- [pee](http://example.org/diff-render#p) EquivalentTo [B](http://example.org/diff-render#q) 

-  Functional: [pee](http://example.org/diff-render#p) 

- [pee](http://example.org/diff-render#p) InverseOf [B](http://example.org/diff-render#q) 

- [pee](http://example.org/diff-render#p) Domain [alpha](http://example.org/diff-render#A) 

- [pee](http://example.org/diff-render#p) Range [alpha](http://example.org/diff-render#A) 

- [pee](http://example.org/diff-render#p) o [B](http://example.org/diff-render#q) SubPropertyOf: [pee](http://example.org/diff-render#p) 

-  Transitive: [pee](http://example.org/diff-render#p) 


### sea `http://example.org/diff-render#C`

#### Added
- Class: [sea](http://example.org/diff-render#C) 

- [sea](http://example.org/diff-render#C) [label](http://www.w3.org/2000/01/rdf-schema#label) "cee"@en 

- [sea](http://example.org/diff-render#C) [label](http://www.w3.org/2000/01/rdf-schema#label) "sea" 

- [sea](http://example.org/diff-render#C) [label](http://www.w3.org/2000/01/rdf-schema#label) "typed"^^[string](http://www.w3.org/2001/XMLSchema#string) 

- [sea](http://example.org/diff-render#C) SubClassOf [dee](http://example.org/diff-render#d) some {"a" , 2 , "b"^^[string](http://www.w3.org/2001/XMLSchema#string)} 

- [sea](http://example.org/diff-render#C) SubClassOf [dee](http://example.org/diff-render#d) some [integer](http://www.w3.org/2001/XMLSchema#integer)[>= 1 , < 9] 


### tee `http://example.org/diff-render#t`

#### Added
- [tee](http://example.org/diff-render#t) 

- [tee](http://example.org/diff-render#t) [label](http://www.w3.org/2000/01/rdf-schema#label) "tee" 

