# Ontology comparison

## Left
- Ontology IRI: `http://example.org/diff-render`
- Version IRI: `http://example.org/diff-render/v1`
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/diff-render-left.ofn`

## Right
- Ontology IRI: `http://example.org/test.owl`
- Version IRI: *None*
- Loaded from: `file:/home/user/owlmake/tests/fixtures/robot-1.9.11/annotated-terms.omn.ofn`

### Ontology imports 



### Ontology annotations 



### Z `http://example.org/diff-render#Z`
#### Removed
- Class: [Z](http://example.org/diff-render#Z) 



### abbreviation `http://example.org/EX_0000900`

#### Added
- AnnotationProperty: [abbreviation](http://example.org/EX_0000900) 

- [abbreviation](http://example.org/EX_0000900) [label](http://www.w3.org/2000/01/rdf-schema#label) "abbreviation" 


### anyURI `http://www.w3.org/2001/XMLSchema#anyURI`

#### Added
- [anyURI](http://www.w3.org/2001/XMLSchema#anyURI) 


### comment `http://www.w3.org/2000/01/rdf-schema#comment`

#### Added
- AnnotationProperty: [comment](http://www.w3.org/2000/01/rdf-schema#comment) 


### contributor `http://purl.org/dc/terms/contributor`

#### Added
- AnnotationProperty: [contributor](http://purl.org/dc/terms/contributor) 


### definition `http://purl.obolibrary.org/obo/IAO_0000115`

#### Added
- AnnotationProperty: [definition](http://purl.obolibrary.org/obo/IAO_0000115) 

- [definition](http://purl.obolibrary.org/obo/IAO_0000115) [label](http://www.w3.org/2000/01/rdf-schema#label) "definition" 


### definition source `http://purl.obolibrary.org/obo/IAO_0000119`

#### Added
- AnnotationProperty: [definition source](http://purl.obolibrary.org/obo/IAO_0000119) 

- [definition source](http://purl.obolibrary.org/obo/IAO_0000119) [label](http://www.w3.org/2000/01/rdf-schema#label) "definition source" 


### example of usage `http://purl.obolibrary.org/obo/IAO_0000112`

#### Added
- AnnotationProperty: [example of usage](http://purl.obolibrary.org/obo/IAO_0000112) 

- [example of usage](http://purl.obolibrary.org/obo/IAO_0000112) [label](http://www.w3.org/2000/01/rdf-schema#label) "example of usage" 


### hasDbXref `http://www.geneontology.org/formats/oboInOwl#hasDbXref`

#### Added
- AnnotationProperty: [hasDbXref](http://www.geneontology.org/formats/oboInOwl#hasDbXref) 


### hasExactSynonym `http://www.geneontology.org/formats/oboInOwl#hasExactSynonym`

#### Added
- AnnotationProperty: [hasExactSynonym](http://www.geneontology.org/formats/oboInOwl#hasExactSynonym) 


### hasRelatedSynonym `http://www.geneontology.org/formats/oboInOwl#hasRelatedSynonym`

#### Added
- AnnotationProperty: [hasRelatedSynonym](http://www.geneontology.org/formats/oboInOwl#hasRelatedSynonym) 


### hasSynonymType `http://www.geneontology.org/formats/oboInOwl#hasSynonymType`

#### Added
- AnnotationProperty: [hasSynonymType](http://www.geneontology.org/formats/oboInOwl#hasSynonymType) 


### label `http://www.w3.org/2000/01/rdf-schema#label`

#### Added
- AnnotationProperty: [label](http://www.w3.org/2000/01/rdf-schema#label) 


### part of `http://example.org/EX_0000100`

#### Added
- ObjectProperty: [part of](http://example.org/EX_0000100) 

- [part of](http://example.org/EX_0000100) [label](http://www.w3.org/2000/01/rdf-schema#label) "part of" 


### source `http://purl.org/dc/terms/source`

#### Added
- AnnotationProperty: [source](http://purl.org/dc/terms/source) 


### string `http://www.w3.org/2001/XMLSchema#string`

#### Added
- [string](http://www.w3.org/2001/XMLSchema#string) 


### thing A `http://example.org/EX_0000001`

#### Added
- Class: [thing A](http://example.org/EX_0000001) 

- [thing A](http://example.org/EX_0000001) [label](http://www.w3.org/2000/01/rdf-schema#label) "thing A" 


### thing B `http://example.org/EX_0000002`

#### Added
- Class: [thing B](http://example.org/EX_0000002) 

- [thing B](http://example.org/EX_0000002) [hasDbXref](http://www.geneontology.org/formats/oboInOwl#hasDbXref) "EXT:123" 

- [thing B](http://example.org/EX_0000002) [hasRelatedSynonym](http://www.geneontology.org/formats/oboInOwl#hasRelatedSynonym) "thing of type B" 

- [thing B](http://example.org/EX_0000002) [definition](http://purl.obolibrary.org/obo/IAO_0000115) "A thing A that is part of a thing C." 
  - [definition source](http://purl.obolibrary.org/obo/IAO_0000119) "https://example.org/source/1"^^[anyURI](http://www.w3.org/2001/XMLSchema#anyURI) 

- [thing B](http://example.org/EX_0000002) [example of usage](http://purl.obolibrary.org/obo/IAO_0000112) "An example of thing B in use." 
  - [source](http://purl.org/dc/terms/source) "https://example.org/source/2"^^[anyURI](http://www.w3.org/2001/XMLSchema#anyURI) 

  - [contributor](http://purl.org/dc/terms/contributor) [0000-0000-0000-0000](https://orcid.org/0000-0000-0000-0000) 

  - [comment](http://www.w3.org/2000/01/rdf-schema#comment) "Example note." 

- [thing B](http://example.org/EX_0000002) [hasExactSynonym](http://www.geneontology.org/formats/oboInOwl#hasExactSynonym) "B thing" 
  - [hasSynonymType](http://www.geneontology.org/formats/oboInOwl#hasSynonymType) [abbreviation](http://example.org/EX_0000900) 

- [thing B](http://example.org/EX_0000002) [hasDbXref](http://www.geneontology.org/formats/oboInOwl#hasDbXref) "EXT:456" 
  - [comment](http://www.w3.org/2000/01/rdf-schema#comment) "Cross-reference note." 

- [thing B](http://example.org/EX_0000002) [comment](http://www.w3.org/2000/01/rdf-schema#comment) "A comment about thing B." 

- [thing B](http://example.org/EX_0000002) [label](http://www.w3.org/2000/01/rdf-schema#label) "thing B" 

- [thing B](http://example.org/EX_0000002) SubClassOf [thing A](http://example.org/EX_0000001) 

- [thing B](http://example.org/EX_0000002) SubClassOf [part of](http://example.org/EX_0000100) some [thing C](http://example.org/EX_0000003) 


### thing C `http://example.org/EX_0000003`

#### Added
- Class: [thing C](http://example.org/EX_0000003) 

- [thing C](http://example.org/EX_0000003) [label](http://www.w3.org/2000/01/rdf-schema#label) "thing C" 

