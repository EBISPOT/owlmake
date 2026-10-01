//! `make-release-assets.py` — attach files to a GitHub release.
//!
//! The standard build's `public_release` runs it when a repository releases with
//! `public_release: github_python`
//! (`make-release-assets.py --release $(TAGNAME) $(RELEASEFILES)`). It is a
//! script of ODK's that nothing else provides, so owlmake answers to the name,
//! with the script's options and what it prints:
//!
//! - `-r/--repo ORG/REPO` (or `-o/--org` and a bare `--repo`), `-v/--release TAG`;
//! - `-t/--token`, else the first line of `.token` in the working directory;
//! - `-c/--create` makes the release first, and with `-f/--force` replaces one
//!   already there; `--force` also replaces an asset of the same name;
//! - `-k/--dry-run` lists the release's assets and uploads nothing.
//!
//! An asset that is already there and not forced is reported and the upload is
//! attempted all the same, as the script does; GitHub refuses it, and that is
//! the failure.
//!
//! One option is owlmake's own: `--api-url`, for a GitHub that is not
//! `https://api.github.com` (GitHub Enterprise). It is an option and not an
//! environment variable so that where a release goes is written where the
//! release is asked for.

use std::io::Read as _;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

struct Api {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl Api {
    fn request(&self, method: &str, url: &str) -> ureq::Request {
        self.agent
            .request(method, url)
            .set("Authorization", &format!("token {}", self.token))
            .set("Accept", "application/vnd.github+json")
            .set("User-Agent", concat!("owlmake/", env!("CARGO_PKG_VERSION")))
    }

    fn read(what: &str, response: std::result::Result<ureq::Response, ureq::Error>) -> Result<Value> {
        match response {
            Ok(r) => {
                let mut text = String::new();
                r.into_reader().read_to_string(&mut text)?;
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                serde_json::from_str(&text).with_context(|| format!("{what}: reading GitHub's answer"))
            }
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                bail!("{what}: GitHub answered {code}: {}", body.trim())
            }
            Err(e) => Err(anyhow!("{what}: {e}")),
        }
    }

    fn get(&self, what: &str, path: &str) -> Result<Value> {
        Self::read(what, self.request("GET", &format!("{}{path}", self.base)).call())
    }

    /// Every page of a listing.
    fn list(&self, what: &str, path: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for page in 1.. {
            let items = self.get(what, &format!("{path}?per_page=100&page={page}"))?;
            let items = items.as_array().cloned().unwrap_or_default();
            let last = items.len() < 100;
            out.extend(items);
            if last {
                break;
            }
        }
        Ok(out)
    }

    fn delete(&self, what: &str, path: &str) -> Result<()> {
        Self::read(what, self.request("DELETE", &format!("{}{path}", self.base)).call()).map(|_| ())
    }
}

/// The media type an upload is sent as, by the file's extension.
fn media_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "json" => "application/json",
        "owl" | "rdf" | "xml" => "application/xml",
        "gz" | "tgz" => "application/gzip",
        "zip" => "application/zip",
        "txt" | "obo" | "md" => "text/plain",
        "tsv" => "text/tab-separated-values",
        "csv" => "text/csv",
        "ttl" => "text/turtle",
        _ => "application/octet-stream",
    }
}

fn run(argv: &[String]) -> Result<()> {
    let (mut dry_run, mut force, mut create) = (false, false, false);
    let (mut token, mut org): (Option<String>, Option<String>) = (None, None);
    let (mut repo, mut release) = ("mondo".to_string(), "v2018-08-24".to_string());
    let mut api_url = "https://api.github.com".to_string();
    let mut paths: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let mut value = |i: &mut usize| -> Result<String> {
            *i += 1;
            argv.get(*i).cloned().ok_or_else(|| anyhow!("Option '{}' requires an argument.", argv[*i - 1]))
        };
        match argv[i].as_str() {
            "-k" | "--dry-run" => dry_run = true,
            "--no-dry-run" => dry_run = false,
            "-f" | "--force" => force = true,
            "--no-force" => force = false,
            "-c" | "--create" => create = true,
            "--no-create" => create = false,
            "-t" | "--token" => token = Some(value(&mut i)?),
            "-o" | "--org" => org = Some(value(&mut i)?),
            "-r" | "--repo" => repo = value(&mut i)?,
            "-v" | "--release" => release = value(&mut i)?,
            "--api-url" => api_url = value(&mut i)?,
            other if other.starts_with('-') && other.len() > 1 => bail!("No such option: {other}"),
            other => paths.push(other),
        }
        i += 1;
    }
    if let Some((o, r)) = repo.clone().split_once('/') {
        org = Some(o.to_string());
        repo = r.to_string();
    }
    let org = org.unwrap_or_else(|| "monarch-initiative".to_string());
    eprintln!("INFO:root:org={org} repo={repo} rel={release}");
    let token = match token {
        Some(t) => t,
        None => {
            let text = std::fs::read_to_string(".token")
                .context("no --token given, and no .token file to read one from")?;
            eprintln!("INFO:root:Reading token from file");
            text.trim().to_string()
        }
    };
    let api = Api {
        base: api_url.trim_end_matches('/').to_string(),
        token,
        agent: ureq::builder()
            .timeout_connect(std::time::Duration::from_secs(60))
            .timeout_read(std::time::Duration::from_secs(600))
            .build(),
    };
    let releases = format!("/repos/{org}/{repo}/releases");

    if create {
        let message = if release == "current" {
            "Running current release. This will be re-created with each release"
        } else {
            ""
        };
        for existing in api.list("listing releases", &releases)? {
            if existing["tag_name"].as_str() != Some(release.as_str()) {
                continue;
            }
            if force {
                eprintln!("INFO:root:Release already exists - will delete and re-create");
                api.delete("deleting the release", &format!("{releases}/{}", existing["id"]))?;
            } else {
                eprintln!("ERROR:root:Release already exists!");
            }
            eprintln!("INFO:root:Creating release");
        }
        let body = serde_json::json!({
            "tag_name": release, "name": release, "body": message, "draft": false, "prerelease": false,
        });
        Api::read(
            "creating the release",
            api.request("POST", &format!("{}{releases}", api.base))
                .set("Content-Type", "application/json")
                .send_string(&body.to_string()),
        )?;
    }

    let found = api.get("finding the release", &format!("{releases}/tags/{release}"))?;
    let id = found["id"].clone();
    let upload_url = found["upload_url"].as_str().unwrap_or_default();
    let upload_url = upload_url.split('{').next().unwrap_or_default().to_string();

    println!("Existing assets:");
    let existing = api.list("listing assets", &format!("{releases}/{id}/assets"))?;
    for a in &existing {
        println!(
            "Asset: {} Size: {} Downloads: {}",
            a["name"].as_str().unwrap_or_default(),
            a["size"],
            a["download_count"]
        );
    }
    if dry_run {
        println!("DRY RUN");
        return Ok(());
    }
    for path in paths {
        let file = Path::new(path);
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or(path);
        eprintln!("INFO:root:Testing if {name} in the existing assets");
        if let Some(a) = existing.iter().find(|a| a["name"].as_str() == Some(name)) {
            if force {
                eprintln!("INFO:root:{path} already exists; will replace");
                api.delete("deleting the asset", &format!("{releases}/assets/{}", a["id"]))?;
            } else {
                eprintln!("ERROR:root:{path} already exists");
            }
        }
        println!("Uploading: {path}");
        let bytes = std::fs::read(file).with_context(|| format!("reading {path}"))?;
        let uploaded = Api::read(
            &format!("uploading {path}"),
            api.request("POST", &upload_url)
                .query("name", name)
                .query("label", "")
                .set("Content-Type", media_type(file))
                .send_bytes(&bytes),
        )?;
        println!("Uploaded: {} {} from {path}", uploaded["name"].as_str().unwrap_or_default(), uploaded["size"]);
    }
    Ok(())
}

pub fn main(argv: &[String]) -> i32 {
    match run(argv) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e:#}");
            1
        }
    }
}
