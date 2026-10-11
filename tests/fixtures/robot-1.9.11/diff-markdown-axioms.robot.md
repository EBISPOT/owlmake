# Ontology comparison

## Left
- Ontology IRI: `http://example.org/diff-render`
- Version IRI: `http://example.org/diff-render/v1`
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/diff-render-left.ofn`

## Right
- Ontology IRI: `http://example.org/m2`
- Version IRI: *None*
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/diff-markdown-axioms.ofn`

### Ontology imports 



### Ontology annotations 



###  inverse (q) 

#### Added
-  inverse ([q](http://example.org/m2#q)) SubPropertyOf:  inverse ([r](http://example.org/m2#r)) 


###  inverse (r) 

#### Added
-  Irreflexive:  inverse ([r](http://example.org/m2#r)) 


### A `http://example.org/m2#A`

#### Added
- Class: [A](http://example.org/m2#A) 

- [A](http://example.org/m2#A) [an ap](http://example.org/m2#ap) _:genid2147483648 

-  EquivalentClasses: [A](http://example.org/m2#A), [B](http://example.org/m2#B), [C](http://example.org/m2#C) 

- [A](http://example.org/m2#A) EquivalentTo [B](http://example.org/m2#B) or ([C](http://example.org/m2#C) and (not ([A](http://example.org/m2#A)))) 

- [A](http://example.org/m2#A) SubClassOf [d](http://example.org/m2#d) min 2 [string](http://www.w3.org/2001/XMLSchema#string) 

- [A](http://example.org/m2#A) SubClassOf  inverse ([p](http://example.org/m2#p)) only (not ([B](http://example.org/m2#B))) 

- [A](http://example.org/m2#A) SubClassOf not ([p](http://example.org/m2#p) some [B](http://example.org/m2#B)) 

- [A](http://example.org/m2#A) SubClassOf [p](http://example.org/m2#p) exactly 1 ([B](http://example.org/m2#B) or [C](http://example.org/m2#C)) 


### B `http://example.org/m2#B`

#### Added
- Class: [B](http://example.org/m2#B) 

- [B](http://example.org/m2#B) HasKey [d](http://example.org/m2#d) , [e](http://example.org/m2#e) 

- [B](http://example.org/m2#B) SubClassOf [d](http://example.org/m2#d) only  not (([boolean](http://www.w3.org/2001/XMLSchema#boolean) or [integer](http://www.w3.org/2001/XMLSchema#integer))) 

- [B](http://example.org/m2#B) SubClassOf [d](http://example.org/m2#d) some {"x"@en , 1.0 , "10.0"^^[double](http://www.w3.org/2001/XMLSchema#double)} 

- [B](http://example.org/m2#B) SubClassOf  inverse ([p](http://example.org/m2#p)) value [i](http://example.org/m2#i) 


### C `http://example.org/m2#C`

#### Added
- Class: [C](http://example.org/m2#C) 

- [C](http://example.org/m2#C) HasKey [p](http://example.org/m2#p) , [q](http://example.org/m2#q) 

- [C](http://example.org/m2#C) SubClassOf {[i](http://example.org/m2#i) , [j](http://example.org/m2#j) , [k](http://example.org/m2#k)} 


### Rules 

#### Added
-  -> [A](http://example.org/m2#A)(?x) 

- ([integer](http://www.w3.org/2001/XMLSchema#integer) or [string](http://www.w3.org/2001/XMLSchema#string))(?y), swrlb:add(?y, 1, 2) -> [d](http://example.org/m2#d)(?z, ?y) 


### Z `http://example.org/diff-render#Z`
#### Removed
- Class: [Z](http://example.org/diff-render#Z) 



### an ap `http://example.org/m2#ap`

#### Added
- AnnotationProperty: [an ap](http://example.org/m2#ap) 

- [an ap](http://example.org/m2#ap) [label](http://www.w3.org/2000/01/rdf-schema#label) "an ap" 

- [an ap](http://example.org/m2#ap) Domain [the doc](http://example.org/m2/doc) 


### d `http://example.org/m2#d`

#### Added
- DataProperty: [d](http://example.org/m2#d) 

-  DisjointProperties: [d](http://example.org/m2#d), [e](http://example.org/m2#e), [f](http://example.org/m2#f) 

- [d](http://example.org/m2#d) DisjointWith [e](http://example.org/m2#e) 

- EquivalentProperties: [d](http://example.org/m2#d), [e](http://example.org/m2#e), [f](http://example.org/m2#f) 

- EquivalentProperties: [d](http://example.org/m2#d), [e](http://example.org/m2#e) 


### e `http://example.org/m2#e`

#### Added
- DataProperty: [e](http://example.org/m2#e) 


### f `http://example.org/m2#f`

#### Added
- DataProperty: [f](http://example.org/m2#f) 


### i `http://example.org/m2#i`

#### Added
- Individual: [i](http://example.org/m2#i) 

-  DifferentIndividuals: [i](http://example.org/m2#i), [j](http://example.org/m2#j), [k](http://example.org/m2#k) 

- [i](http://example.org/m2#i)  inverse ([p](http://example.org/m2#p)) [j](http://example.org/m2#j) 


### j `http://example.org/m2#j`

#### Added
- Individual: [j](http://example.org/m2#j) 


### k `http://example.org/m2#k`

#### Added
- Individual: [k](http://example.org/m2#k) 


### p `http://example.org/m2#p`

#### Added
- ObjectProperty: [p](http://example.org/m2#p) 

-  DisjointProperties: [p](http://example.org/m2#p), [q](http://example.org/m2#q), [r](http://example.org/m2#r) 

-  EquivalentProperties: [p](http://example.org/m2#p), [q](http://example.org/m2#q), [r](http://example.org/m2#r) 

-  InverseFunctional: [p](http://example.org/m2#p) 

-  Symmetric: [p](http://example.org/m2#p) 


### q `http://example.org/m2#q`

#### Added
- ObjectProperty: [q](http://example.org/m2#q) 

-  Asymmetric: [q](http://example.org/m2#q) 


### r `http://example.org/m2#r`

#### Added
- ObjectProperty: [r](http://example.org/m2#r) 

-  Reflexive: [r](http://example.org/m2#r) 


### the doc `http://example.org/m2/doc`

#### Added
- [the doc](http://example.org/m2/doc) [an ap](http://example.org/m2#ap) _:genid2147483648 

- [the doc](http://example.org/m2/doc) [label](http://www.w3.org/2000/01/rdf-schema#label) "the doc" 

