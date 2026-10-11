## [Thing](http://www.w3.org/2002/07/owl#Thing) SubClassOf [Nothing](http://www.w3.org/2002/07/owl#Nothing) ##

  - [p](http://example.org/r#p)(?x, ?y), [d](http://example.org/r#d)(?x, ?v), [integer](http://www.w3.org/2001/XMLSchema#integer)(?v), (not ([B](http://example.org/r#B)))(?x),  DifferentFrom (?x, ?y),  SameAs (?y, [y](http://example.org/r#y)), [A](http://example.org/r#A)(?x) -> [C](http://example.org/r#C)(?y), [d](http://example.org/r#d)(?y, 7)
  - [x](http://example.org/r#x) Type not ([B](http://example.org/r#B))
  - [x](http://example.org/r#x) DifferentFrom [y](http://example.org/r#y)
  - [C](http://example.org/r#C) DisjointWith [D](http://example.org/r#D)
  - [y](http://example.org/r#y) Type [D](http://example.org/r#D)
  - [x](http://example.org/r#x) [d](http://example.org/r#d) 5
  - [x](http://example.org/r#x) Type [A](http://example.org/r#A)
  - [x](http://example.org/r#x) [p](http://example.org/r#p) [y](http://example.org/r#y)

# Axiom Impact 
## Axioms used 1 times
- [C](http://example.org/r#C) DisjointWith [D](http://example.org/r#D) [rule4]
- [x](http://example.org/r#x) Type [A](http://example.org/r#A) [rule4]
- [x](http://example.org/r#x) Type not ([B](http://example.org/r#B)) [rule4]
- [y](http://example.org/r#y) Type [D](http://example.org/r#D) [rule4]
- [x](http://example.org/r#x) DifferentFrom [y](http://example.org/r#y) [rule4]
- [x](http://example.org/r#x) [p](http://example.org/r#p) [y](http://example.org/r#y) [rule4]
- [x](http://example.org/r#x) [d](http://example.org/r#d) 5 [rule4]
- [p](http://example.org/r#p)(?x, ?y), [d](http://example.org/r#d)(?x, ?v), [integer](http://www.w3.org/2001/XMLSchema#integer)(?v), (not ([B](http://example.org/r#B)))(?x),  DifferentFrom (?x, ?y),  SameAs (?y, [y](http://example.org/r#y)), [A](http://example.org/r#A)(?x) -> [C](http://example.org/r#C)(?y), [d](http://example.org/r#d)(?y, 7) [rule4]



# Ontologies used: 
- rule4 (http://example.org/rule4)
