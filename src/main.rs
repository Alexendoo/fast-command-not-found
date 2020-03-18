use anyhow::{ensure, Context, Result};
use fst::automaton::Levenshtein;
use fst::{IntoStreamer, MapBuilder, Streamer};
use once_cell::unsync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::{Archive, EntryType};

#[derive(Debug, Default)]
struct Package {
    desc: String,
    files: String,
}

type Packages = HashMap<PathBuf, Package>;

fn re(section: &str) -> Regex {
    Regex::new(section).unwrap()
}
const NAME_RE: Lazy<Regex> = Lazy::new(|| re(r"%NAME%\n([^\n]+)"));
const FILES_RE: Lazy<Regex> = Lazy::new(|| re(r"(?s)%FILES%\n(.*)"));

fn get_section<'a>(re: &Regex, text: &'a str) -> Result<&'a str> {
    re.captures(text)
        .context("failed to match")?
        .get(1)
        .map(|capture| capture.as_str())
        .context("no captures")
}

fn read_packages(reader: impl Read) -> Result<Packages> {
    let mut archive = Archive::new(reader);
    let mut packages: HashMap<PathBuf, Package> = HashMap::new();

    for file in archive.entries()? {
        let mut file = file?;

        if file.header().entry_type() != EntryType::Regular {
            continue;
        }

        let path = file.path()?;
        let parent = path.parent().context("invalid parent")?.to_owned();
        let file_name = path.file_name().context("invalid filename")?;

        let package = packages.entry(parent).or_default();
        let write = match file_name.to_str() {
            Some("desc") => &mut package.desc,
            Some("files") => &mut package.files,
            _ => continue,
        };
        file.read_to_string(write)?;
    }

    for (path, package) in &packages {
        ensure!(
            !package.desc.is_empty(),
            "expected package.desc: {:?}",
            path
        );
        ensure!(
            !package.files.is_empty(),
            "expected package.files: {:?}",
            path
        );
    }

    Ok(packages)
}

#[derive(Serialize, Deserialize, Debug)]
struct Provider<'a> {
    package_name: &'a str,
    path: &'a str,
}

type Providers<'a> = Vec<Provider<'a>>;
type ProviderMap<'a> = BTreeMap<&'a str, Providers<'a>>;

fn put_providers<'a>(packages: &'a Packages, out: &mut ProviderMap<'a>) -> Result<()> {
    for package in packages.values() {
        let name = get_section(&NAME_RE, &package.desc)?;
        let files = get_section(&FILES_RE, &package.files)?;

        for file in files.lines() {
            let path = Path::new(file);

            if file.ends_with('/') || !file.contains("/bin/") {
                continue;
            }

            let bin_name = path
                .file_name()
                .context("invalid filename")?
                .to_str()
                .context("invalid UTF-8")?;

            out.entry(bin_name).or_default().push(Provider {
                package_name: name,
                path: file,
            });
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    let core = File::open("./core.files")?;

    let packages = read_packages(core)?;
    let mut bins = BTreeMap::new();
    put_providers(&packages, &mut bins)?;

    let mut builder = MapBuilder::memory();
    let mut buffer: Vec<u8> = Vec::new();

    for (bin, providers) in bins {
        let index = buffer.len() as u64;
        bincode::serialize_into(&mut buffer, &providers)?;

        builder.insert(bin, index)?;
    }

    let map = builder.into_map();

    let query = Levenshtein::new("ls", 0)?;

    let mut stream = map.search(&query).into_stream();
    while let Some((k, v)) = stream.next() {
        let binary = String::from_utf8_lossy(k);

        let start = v as usize;
        let packages: Providers = bincode::deserialize(&buffer[start..])?;
    }

    // eprintln!("map = {:#?}", map);

    Ok(())
}
