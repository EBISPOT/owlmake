//! The prefixes a command reads the CURIEs it is given with.
//!
//! A CURIE on a command line — an option's value, a line of a term file, a
//! template cell, a row of a rename table — is read with this context and
//! nothing else: the built-in map of OBO prefixes (`obo_context.jsonld`), or the
//! `--prefixes` file in its place, or none under `--noprefixes`, and then what
//! the command line binds with `--add-prefixes`, `--prefix` and `--add-prefix`.
//! The prefixes a document declares say how that document writes its IRIs; they
//! never decide what a command line names.

/// What a command line reads CURIEs with. See the module documentation.
#[derive(Clone, Debug)]
pub struct Context {
    /// Whether the built-in map is in it.
    builtin: bool,
    /// The bindings the command line makes, in the order it makes them; a name
    /// bound again keeps its place and takes the later namespace.
    bound: Vec<(String, String)>,
}

impl Default for Context {
    fn default() -> Self {
        Context { builtin: true, bound: Vec::new() }
    }
}

impl Context {
    /// A context without the built-in map: `--noprefixes`, or a `--prefixes`
    /// file, whose bindings take its place.
    pub fn without_builtin() -> Self {
        Context { builtin: false, bound: Vec::new() }
    }

    /// Bind `name` to `namespace`.
    pub fn bind(&mut self, name: &str, namespace: &str) {
        match self.bound.iter_mut().find(|(p, _)| p == name) {
            Some(slot) => slot.1 = namespace.to_string(),
            None => self.bound.push((name.to_string(), namespace.to_string())),
        }
    }

    /// The namespace `prefix` names.
    pub fn namespace(&self, prefix: &str) -> Option<&str> {
        self.bound
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, ns)| ns.as_str())
            .or_else(|| {
                self.builtin
                    .then(|| crate::report::obo_context_map().get(prefix).map(String::as_str))
                    .flatten()
            })
    }

    /// Every binding, in order: the built-in map's in its own order, then the
    /// command line's, a name the command line binds again keeping its place.
    pub fn entries(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        if self.builtin {
            out.extend(crate::report::obo_context_prefixes().iter().cloned());
        }
        for (name, ns) in &self.bound {
            match out.iter_mut().find(|(p, _)| p == name) {
                Some(slot) => slot.1 = ns.clone(),
                None => out.push((name.clone(), ns.clone())),
            }
        }
        out
    }

    /// The bindings as a prefix map, for a reader that takes one.
    pub fn prefix_mapping(&self) -> horned_owl::curie::PrefixMapping {
        let mut map = horned_owl::curie::PrefixMapping::default();
        for (name, ns) in self.entries() {
            let _ = map.add_prefix(&name, &ns);
        }
        map
    }

    /// The IRI `term` names, or `None` when it names none.
    ///
    /// An IRI is itself (a `scheme:` whose rest begins `//`, or a blank node
    /// label `_:x`); `prefix:local` with a bound prefix is the prefix's
    /// namespace and the local part, and with an unbound one is taken as an IRI
    /// whose scheme is the prefix; a bare name is the namespace it is bound to.
    /// What comes out names an IRI only when a URL can be made of it — a scheme
    /// the platform reads (`http`, `https`, `ftp`, `file`, `mailto`, `jrt`, and
    /// `jar` with its `!/`), or a well-formed `urn:` — so an unbound `ex:A`
    /// names nothing.
    pub fn iri(&self, term: &str) -> Option<String> {
        let iri = match term.split_once(':') {
            Some((prefix, rest)) if prefix == "_" || rest.starts_with("//") => term.to_string(),
            Some((prefix, rest)) => match self.namespace(prefix) {
                Some(ns) => format!("{ns}{rest}"),
                None => term.to_string(),
            },
            None => self.namespace(term)?.to_string(),
        };
        is_url(&iri).then_some(iri)
    }
}

/// Whether a URL can be made of `iri`.
fn is_url(iri: &str) -> bool {
    let Some((scheme, rest)) = iri.split_once(':') else { return false };
    let scheme = scheme.to_ascii_lowercase();
    if scheme == "urn" {
        return is_urn(rest);
    }
    match scheme.as_str() {
        "http" | "https" | "ftp" | "file" | "mailto" | "jrt" => true,
        "jar" => rest.contains("!/"),
        _ => false,
    }
}

/// Whether `rest`, the text after `urn:`, is a URN's: a namespace of a letter or
/// digit and up to 31 more letters, digits and hyphens, a colon, and one or more
/// characters a URN allows or `%`-escapes.
fn is_urn(rest: &str) -> bool {
    let Some((nid, nss)) = rest.split_once(':') else { return false };
    let mut nid_chars = nid.chars();
    let nid_ok = nid_chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && nid.len() <= 32
        && nid_chars.all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !nid_ok || nss.is_empty() {
        return false;
    }
    let bytes = nss.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            let hex = |j: usize| bytes.get(j).is_some_and(|c| c.is_ascii_hexdigit());
            if !(hex(i + 1) && hex(i + 2)) {
                return false;
            }
            i += 3;
            continue;
        }
        if !(b.is_ascii_alphanumeric() || b"()+,-.:=@;$_!*'".contains(&b)) {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CURIE is read with the built-in map and what the command line binds,
    /// and an IRI as itself; a prefix bound nowhere, a bare word and a
    /// malformed URN name nothing.
    #[test]
    fn a_term_names_the_iri_the_context_makes_of_it() {
        let mut c = Context::default();
        assert_eq!(c.iri("oboInOwl:date").as_deref(), Some("http://www.geneontology.org/formats/oboInOwl#date"));
        assert_eq!(c.iri("GO:0000001").as_deref(), Some("http://purl.obolibrary.org/obo/GO_0000001"));
        assert_eq!(c.iri("dc:title").as_deref(), Some("http://purl.org/dc/terms/title"));
        assert_eq!(c.iri("http://example.org/x").as_deref(), Some("http://example.org/x"));
        assert_eq!(c.iri("urn:isbn:0451450523").as_deref(), Some("urn:isbn:0451450523"));
        assert_eq!(c.iri("GO").as_deref(), Some("http://purl.obolibrary.org/obo/GO_"));
        assert_eq!(c.iri("ex:A"), None);
        assert_eq!(c.iri("A"), None);
        assert_eq!(c.iri("urn:x"), None);
        c.bind("ex", "http://example.org/ex#");
        assert_eq!(c.iri("ex:A").as_deref(), Some("http://example.org/ex#A"));
        c.bind("GO", "http://example.org/go/");
        assert_eq!(c.iri("GO:1").as_deref(), Some("http://example.org/go/1"));
        let none = Context::without_builtin();
        assert_eq!(none.iri("GO:1"), None);
        assert_eq!(none.iri("http://example.org/x").as_deref(), Some("http://example.org/x"));
    }

    /// The bindings come out in order, the built-in map's first, and a name the
    /// command line binds again keeps the place it had.
    #[test]
    fn a_rebound_name_keeps_its_place() {
        let mut c = Context::default();
        c.bind("ex", "http://example.org/ex#");
        c.bind("obo", "http://example.org/obo/");
        let entries = c.entries();
        assert_eq!(entries[0], ("obo".to_string(), "http://example.org/obo/".to_string()));
        assert_eq!(entries.last().unwrap(), &("ex".to_string(), "http://example.org/ex#".to_string()));
    }
}
