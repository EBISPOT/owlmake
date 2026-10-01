//! `context2csv` — a JSON-LD context as a prefix table.
//!
//! The standard build makes the prefix table the SQL database export reads from
//! the repository's context file (`context2csv < context.json > prefixes.csv`).
//! It is a helper of ODK's that nothing else provides, so owlmake answers to the
//! name: standard input's `@context`, one `prefix,base` line per entry, in the
//! order the file has them, under that header.

use std::io::Read;

/// A value as Python prints it inside an f-string, which is what the helper
/// does with a context entry that is not a plain string.
fn printed(v: &serde_json::Value, nested: bool) -> String {
    use serde_json::Value;
    match v {
        Value::String(s) if nested => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => {
            format!("[{}]", items.iter().map(|i| printed(i, true)).collect::<Vec<_>>().join(", "))
        }
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", printed(&Value::String(k.clone()), true), printed(v, true)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn main(_argv: &[String]) -> i32 {
    let mut text = String::new();
    let read = std::io::stdin().read_to_string(&mut text).is_ok();
    let Some(context) = read.then(|| serde_json::from_str::<serde_json::Value>(&text).ok()).flatten() else {
        eprintln!("Cannot read context file");
        return 1;
    };
    let Some(entries) = context.get("@context") else {
        eprintln!("No @context in supposed context file");
        return 1;
    };
    let mut out = String::from("prefix,base\n");
    for (prefix, base) in entries.as_object().into_iter().flatten() {
        out.push_str(&format!("{prefix},{}\n", printed(base, false)));
    }
    print!("{out}");
    0
}
