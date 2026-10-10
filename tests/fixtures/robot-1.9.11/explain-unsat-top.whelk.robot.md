## [Thing](http://www.w3.org/2002/07/owl#Thing) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  -  Reflexive: [R_1](http://purl.obolibrary.org/obo/R_1)
  - [X_1](http://purl.obolibrary.org/obo/X_1) SubClassOf not ([X_2](http://purl.obolibrary.org/obo/X_2))
  - [R_1](http://purl.obolibrary.org/obo/R_1) Range [X_2](http://purl.obolibrary.org/obo/X_2)
  - [X_2](http://purl.obolibrary.org/obo/X_2) SubClassOf [X_1](http://purl.obolibrary.org/obo/X_1)


## [X_1](http://purl.obolibrary.org/obo/X_1) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [X_1](http://purl.obolibrary.org/obo/X_1) SubClassOf not ([X_2](http://purl.obolibrary.org/obo/X_2))
  -  Reflexive: [R_1](http://purl.obolibrary.org/obo/R_1)
  - [R_1](http://purl.obolibrary.org/obo/R_1) Range [X_2](http://purl.obolibrary.org/obo/X_2)


## [X_2](http://purl.obolibrary.org/obo/X_2) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [X_2](http://purl.obolibrary.org/obo/X_2) SubClassOf [X_1](http://purl.obolibrary.org/obo/X_1)
    - [X_1](http://purl.obolibrary.org/obo/X_1) SubClassOf not ([X_2](http://purl.obolibrary.org/obo/X_2))

# Axiom Impact 
## Axioms used 3 times
- [X_1](http://purl.obolibrary.org/obo/X_1) SubClassOf not ([X_2](http://purl.obolibrary.org/obo/X_2)) [explain-unsat-top.owl]

## Axioms used 2 times
- [X_2](http://purl.obolibrary.org/obo/X_2) SubClassOf [X_1](http://purl.obolibrary.org/obo/X_1) [explain-unsat-top.owl]
-  Reflexive: [R_1](http://purl.obolibrary.org/obo/R_1) [explain-unsat-top.owl]
- [R_1](http://purl.obolibrary.org/obo/R_1) Range [X_2](http://purl.obolibrary.org/obo/X_2) [explain-unsat-top.owl]



# Ontologies used: 
- explain-unsat-top.owl (http://purl.obolibrary.org/obo/explain-unsat-top.owl)
