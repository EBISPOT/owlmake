# Ontology comparison

## Left
- Ontology IRI: `http://example.org/diff-render`
- Version IRI: `http://example.org/diff-render/v1`
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/diff-render-left.ofn`

## Right
- Ontology IRI: `http://example.org/md`
- Version IRI: *None*
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/diff-markdown.ofn`

### Ontology imports 



### Ontology annotations 

#### Added
- [label](http://www.w3.org/2000/01/rdf-schema#label) "onto" 
  - [comment](http://www.w3.org/2000/01/rdf-schema#comment) "nested" 

- [comment](http://www.w3.org/2000/01/rdf-schema#comment) "a&lt;b&gt;&amp;amp;" 


###  inverse (pee) 

#### Added
-  inverse ([pee](http://example.org/md#p)) SubPropertyOf: [q](http://example.org/md#q) 


###  inverse (q) 

#### Added
-  Functional:  inverse ([q](http://example.org/md#q)) 


### (integer or string) 

#### Added
-  


### GCIs 

#### Added
- [pee](http://example.org/md#p) some [same](http://example.org/md#A) EquivalentTo [q](http://example.org/md#q) some [same](http://example.org/md#B) 

- [pee](http://example.org/md#p) some [same](http://example.org/md#A) HasKey [pee](http://example.org/md#p) 

- [same](http://example.org/md#A) or [same](http://example.org/md#B) SubClassOf [a<b & "c" [x]](http://example.org/md#C) 


### Rules 

#### Added
- ([pee](http://example.org/md#p) some [same](http://example.org/md#A))(?<http://example.org/md#v>), [integer](http://www.w3.org/2001/XMLSchema#integer)(?<http://example.org/md#x>), <http://example.org/md#myBuiltin>(?<http://example.org/md#x>, 1), [pee](http://example.org/md#p)(?<http://example.org/md#v>, [i](http://example.org/md#i)), [d](http://example.org/md#d)(?<http://example.org/md#v>, "lit") ->  DifferentFrom (?<http://example.org/md#v>, [i](http://example.org/md#i)) 


### Z `http://example.org/diff-render#Z`
#### Removed
- Class: [Z](http://example.org/diff-render#Z) 



### _:genid2147483649 

#### Added
- _:genid2147483649 [comment](http://www.w3.org/2000/01/rdf-schema#comment) "anon y" 

- _:genid2147483649 DifferentFrom _:genid2147483650 


### _:genid2147483650 

#### Added
- _:genid2147483650 Type [same](http://example.org/md#A) 


### a<b & "c" [x] `http://example.org/md#C`

#### Added
- Class: [a<b & "c" [x]](http://example.org/md#C) 

- [a<b & "c" [x]](http://example.org/md#C) [label](http://www.w3.org/2000/01/rdf-schema#label) "a&lt;b &amp; &quot;c&quot; [x]" 

- [a<b & "c" [x]](http://example.org/md#C) SubClassOf [d](http://example.org/md#d) only  not ({1}) 

- [a<b & "c" [x]](http://example.org/md#C) SubClassOf [d](http://example.org/md#d) only  not [integer](http://www.w3.org/2001/XMLSchema#integer) 

- [a<b & "c" [x]](http://example.org/md#C) SubClassOf [d](http://example.org/md#d) value 1.5f 

- [a<b & "c" [x]](http://example.org/md#C) SubClassOf [d](http://example.org/md#d) value "2.5"^^[double](http://www.w3.org/2001/XMLSchema#double) 

- [a<b & "c" [x]](http://example.org/md#C) SubClassOf [d](http://example.org/md#d) value "&lt;&amp;&gt;"@en 

- [a<b & "c" [x]](http://example.org/md#C) SubClassOf [d](http://example.org/md#d) value true 

- [a<b & "c" [x]](http://example.org/md#C) SubClassOf [d](http://example.org/md#d) some ([integer](http://www.w3.org/2001/XMLSchema#integer) and [integer](http://www.w3.org/2001/XMLSchema#integer)[> 0]) 

- [a<b & "c" [x]](http://example.org/md#C) SubClassOf {[i](http://example.org/md#i) , _:genid2147483648} 

- [a<b & "c" [x]](http://example.org/md#C) SubClassOf [same](http://example.org/md#A) 
  - [label](http://www.w3.org/2000/01/rdf-schema#label) "c2" 

  - [comment](http://www.w3.org/2000/01/rdf-schema#comment) "c1" 
    - [comment](http://www.w3.org/2000/01/rdf-schema#comment) "deep" 

  - [seeAlso](http://www.w3.org/2000/01/rdf-schema#seeAlso) [same](http://example.org/md#B) 

  - [comment](http://www.w3.org/2000/01/rdf-schema#comment) "c3"^^[string](http://www.w3.org/2001/XMLSchema#string) 


### d `http://example.org/md#d`

#### Added
- DataProperty: [d](http://example.org/md#d) 


### i `http://example.org/md#i`

#### Added
- Individual: [i](http://example.org/md#i) 


### integer `http://www.w3.org/2001/XMLSchema#integer`

#### Added
-  


### iri-subject `http://example.org/md/iri-subject`

#### Added
- [iri-subject](http://example.org/md/iri-subject) [comment](http://www.w3.org/2000/01/rdf-schema#comment) "about an IRI" 


### pee `http://example.org/md#p`

#### Added
- ObjectProperty: [pee](http://example.org/md#p) 

- [pee](http://example.org/md#p) [label](http://www.w3.org/2000/01/rdf-schema#label) "pee" 


### q `http://example.org/md#q`

#### Added
- ObjectProperty: [q](http://example.org/md#q) 

- [q](http://example.org/md#q) InverseOf  inverse ([pee](http://example.org/md#p)) 


### r `http://example.org/md#r`

#### Added
- ObjectProperty: [r](http://example.org/md#r) 

- [r](http://example.org/md#r) EquivalentTo  inverse ([pee](http://example.org/md#p)) 


### same `http://example.org/md#B`

#### Added
- Class: [same](http://example.org/md#B) 

- [same](http://example.org/md#B) [label](http://www.w3.org/2000/01/rdf-schema#label) "same" 

- [same](http://example.org/md#B) SubClassOf [d](http://example.org/md#d) exactly 1 [boolean](http://www.w3.org/2001/XMLSchema#boolean) 

- [same](http://example.org/md#B) SubClassOf [d](http://example.org/md#d) max 3 [Literal](http://www.w3.org/2000/01/rdf-schema#Literal) 

- [same](http://example.org/md#B) SubClassOf [pee](http://example.org/md#p) exactly 2 [Thing](http://www.w3.org/2002/07/owl#Thing) 

- [same](http://example.org/md#B) SubClassOf [pee](http://example.org/md#p) max 1 ([same](http://example.org/md#A) and [a<b & "c" [x]](http://example.org/md#C)) 

- [same](http://example.org/md#B) SubClassOf [pee](http://example.org/md#p) min 1 [a<b & "c" [x]](http://example.org/md#C) 


### same `http://example.org/md#A`

#### Added
- Class: [same](http://example.org/md#A) 

- [same](http://example.org/md#A) [label](http://www.w3.org/2000/01/rdf-schema#label) "same" 

-  DisjointClasses: [same](http://example.org/md#A), [same](http://example.org/md#B), [a<b & "c" [x]](http://example.org/md#C) 

- [same](http://example.org/md#A) SubClassOf [pee](http://example.org/md#p) only 
([same](http://example.org/md#B) or (not ([a<b & "c" [x]](http://example.org/md#C)))) 

- [same](http://example.org/md#A) SubClassOf [pee](http://example.org/md#p) Self  

- [same](http://example.org/md#A) SubClassOf [pee](http://example.org/md#p) some 
([same](http://example.org/md#B) and [a<b & "c" [x]](http://example.org/md#C)) 

- [same](http://example.org/md#A) SubClassOf [pee](http://example.org/md#p) some ([q](http://example.org/md#q) some [a<b & "c" [x]](http://example.org/md#C)) 


### t `http://example.org/md#t`

#### Added
- [t](http://example.org/md#t) 


### t2 `http://example.org/md#t2`

#### Added
- [t2](http://example.org/md#t2) 

