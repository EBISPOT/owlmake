import subprocess, re, os
HDR='''Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)
Prefix(xml:=<http://www.w3.org/XML/1998/namespace>)
Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)
Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Prefix(obo:=<http://purl.obolibrary.org/obo/>)
Prefix(oio:=<http://www.geneontology.org/formats/oboInOwl#>)

Ontology(<http://example.org/S.owl>
Declaration(Class(obo:X)) Declaration(Class(obo:A)) Declaration(Class(obo:B)) Declaration(Class(obo:D))
Declaration(ObjectProperty(obo:R)) Declaration(ObjectProperty(obo:S)) Declaration(ObjectProperty(obo:R2))
Declaration(AnnotationProperty(oio:source))
'''
TAIL='SubClassOf(Annotation(oio:source "s") obo:X ObjectSomeValuesFrom(obo:R2 obo:D))\n)\n'
cases={
 'nested':      'SubClassOf(obo:X ObjectSomeValuesFrom(obo:R ObjectSomeValuesFrom(obo:S obo:A)))',
 'allvalues':   'SubClassOf(obo:X ObjectAllValuesFrom(obo:R obo:A))',
 'mincard':     'SubClassOf(obo:X ObjectMinCardinality(2 obo:R obo:A))',
 'union':       'SubClassOf(obo:X ObjectUnionOf(obo:A obo:B))',
 'complement':  'SubClassOf(obo:X ObjectComplementOf(obo:A))',
 'hasvalue':    'SubClassOf(obo:X ObjectHasSelf(obo:R))',
 'inverse':     'SubClassOf(obo:X ObjectSomeValuesFrom(ObjectInverseOf(obo:R) obo:A))',
 'twoannot':    'SubClassOf(Annotation(oio:source "a") obo:X ObjectSomeValuesFrom(obo:R obo:A))\nSubClassOf(Annotation(oio:source "b") obo:X ObjectSomeValuesFrom(obo:S obo:B))',
 'disjoint3':   'DisjointClasses(obo:X obo:A obo:B)',
}
def maxg(p):
    t=open(p,encoding='utf-8',errors='replace').read()
    g=[int(m) for m in re.findall(r'genid(\d+)',t)]
    return max(g) if g else 0
for name,ax in cases.items():
    open(f's_{name}.ofn','w').write(HDR+ax+'\n'+TAIL)
    subprocess.run(['/root/odk16bin/robot','convert','-i',f's_{name}.ofn','-f','owl','-o',f's_{name}_rb.owl'],
                   env={**os.environ,'JAVA_TOOL_OPTIONS':''},capture_output=True)
    subprocess.run(['/home/user/owlmake/target/release/om','convert','-i',f's_{name}.ofn','-f','owl','-o',f's_{name}_om.owl'],
                   env={**os.environ,'OM_OWLRDF':'1'},capture_output=True)
    r,o=maxg(f's_{name}_rb.owl'),maxg(f's_{name}_om.owl')
    print(f"  {name:12} robot={r:3}  om={o:3}  {'MISMATCH <<<' if r!=o else ''}")
