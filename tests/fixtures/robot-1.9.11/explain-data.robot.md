## [U3](http://example.org/explain-data#U3) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [U3](http://example.org/explain-data#U3) SubClassOf [r](http://example.org/explain-data#r) only  not ({"a"})
  - [U3](http://example.org/explain-data#U3) SubClassOf [r](http://example.org/explain-data#r) exactly 2 {"a" , "b"}


## [U5](http://example.org/explain-data#U5) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [U5](http://example.org/explain-data#U5) SubClassOf [p](http://example.org/explain-data#p) some [Literal](http://www.w3.org/2000/01/rdf-schema#Literal)
    - [p](http://example.org/explain-data#p) Domain [X](http://example.org/explain-data#X)
  - [U5](http://example.org/explain-data#U5) DisjointWith [X](http://example.org/explain-data#X)


## [U4](http://example.org/explain-data#U4) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [U4](http://example.org/explain-data#U4) SubClassOf [s](http://example.org/explain-data#s) some ([integer](http://www.w3.org/2001/XMLSchema#integer) and [string](http://www.w3.org/2001/XMLSchema#string))


## [U2](http://example.org/explain-data#U2) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [U2](http://example.org/explain-data#U2) SubClassOf [q](http://example.org/explain-data#q) only [string](http://www.w3.org/2001/XMLSchema#string)
  - [U2](http://example.org/explain-data#U2) SubClassOf [q](http://example.org/explain-data#q) min 2 [Literal](http://www.w3.org/2000/01/rdf-schema#Literal)
  - [U2](http://example.org/explain-data#U2) SubClassOf [q](http://example.org/explain-data#q) max 1 [string](http://www.w3.org/2001/XMLSchema#string)


## [U8](http://example.org/explain-data#U8) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [U8](http://example.org/explain-data#U8) SubClassOf [r](http://example.org/explain-data#r) value "b"
    -  Functional: [r](http://example.org/explain-data#r)
  - [U8](http://example.org/explain-data#U8) SubClassOf [r](http://example.org/explain-data#r) value "a"


## [U6](http://example.org/explain-data#U6) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [U6](http://example.org/explain-data#U6) SubClassOf [q](http://example.org/explain-data#q) some [integer](http://www.w3.org/2001/XMLSchema#integer)
    - [q](http://example.org/explain-data#q) Range: [string](http://www.w3.org/2001/XMLSchema#string)


## [U7](http://example.org/explain-data#U7) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [U7](http://example.org/explain-data#U7) SubClassOf [s](http://example.org/explain-data#s) some [integer](http://www.w3.org/2001/XMLSchema#integer)
    - [s](http://example.org/explain-data#s) SubPropertyOf: [q](http://example.org/explain-data#q)
      - [q](http://example.org/explain-data#q) Range: [string](http://www.w3.org/2001/XMLSchema#string)

# Axiom Impact 
## Axioms used 2 times
- [q](http://example.org/explain-data#q) Range: [string](http://www.w3.org/2001/XMLSchema#string) [explain-data]

## Axioms used 1 times
- [U2](http://example.org/explain-data#U2) SubClassOf [q](http://example.org/explain-data#q) only [string](http://www.w3.org/2001/XMLSchema#string) [explain-data]
- [U2](http://example.org/explain-data#U2) SubClassOf [q](http://example.org/explain-data#q) min 2 [Literal](http://www.w3.org/2000/01/rdf-schema#Literal) [explain-data]
- [U2](http://example.org/explain-data#U2) SubClassOf [q](http://example.org/explain-data#q) max 1 [string](http://www.w3.org/2001/XMLSchema#string) [explain-data]
- [U3](http://example.org/explain-data#U3) SubClassOf [r](http://example.org/explain-data#r) only  not ({"a"}) [explain-data]
- [U3](http://example.org/explain-data#U3) SubClassOf [r](http://example.org/explain-data#r) exactly 2 {"a" , "b"} [explain-data]
- [U4](http://example.org/explain-data#U4) SubClassOf [s](http://example.org/explain-data#s) some ([integer](http://www.w3.org/2001/XMLSchema#integer) and [string](http://www.w3.org/2001/XMLSchema#string)) [explain-data]
- [U5](http://example.org/explain-data#U5) SubClassOf [p](http://example.org/explain-data#p) some [Literal](http://www.w3.org/2000/01/rdf-schema#Literal) [explain-data]
- [U6](http://example.org/explain-data#U6) SubClassOf [q](http://example.org/explain-data#q) some [integer](http://www.w3.org/2001/XMLSchema#integer) [explain-data]
- [U7](http://example.org/explain-data#U7) SubClassOf [s](http://example.org/explain-data#s) some [integer](http://www.w3.org/2001/XMLSchema#integer) [explain-data]
- [U8](http://example.org/explain-data#U8) SubClassOf [r](http://example.org/explain-data#r) value "a" [explain-data]
- [U8](http://example.org/explain-data#U8) SubClassOf [r](http://example.org/explain-data#r) value "b" [explain-data]
- [U5](http://example.org/explain-data#U5) DisjointWith [X](http://example.org/explain-data#X) [explain-data]
- [s](http://example.org/explain-data#s) SubPropertyOf: [q](http://example.org/explain-data#q) [explain-data]
-  Functional: [r](http://example.org/explain-data#r) [explain-data]
- [p](http://example.org/explain-data#p) Domain [X](http://example.org/explain-data#X) [explain-data]



# Ontologies used: 
- explain-data (http://example.org/explain-data)
