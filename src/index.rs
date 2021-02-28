use crate::intern::Interner;
use crate::{phf, Span};
use crate::{Header, Provider, HEADER_VERSION};
use anyhow::{ensure, Context, Result};
use bytemuck::{bytes_of, cast_slice, Pod};
use itertools::Itertools;
use std::cmp::Ordering::{Greater, Less};
use std::convert::TryInto;
use std::fs::{self, File};
use std::io::prelude::*;
use std::io::BufReader;
use std::mem::size_of;
use std::process::{Command, Stdio};

fn find_providers(providers: &[Provider], strings: &Interner, target: &str) -> Span {
    fn partition_point(slice: &[Provider], pred: impl Fn(Provider) -> bool) -> usize {
        slice
            .binary_search_by(|&x| if pred(x) { Less } else { Greater })
            .unwrap_err()
    }

    let start = partition_point(providers, |x| strings.get(x.bin) < target);
    let end = providers[start..]
        .iter()
        .position(|x| strings.get(x.bin) != target)
        .map(|pos| pos + start)
        .unwrap_or(providers.len());

    Span {
        start: start as u32,
        end: end as u32,
    }
}

fn byte_len<T: Pod>(slice: &[T]) -> u32 {
    cast_slice::<T, u8>(slice).len().try_into().unwrap()
}

pub fn index(db_path: &str) -> Result<()> {
    let temp_path = format!("{}.tmp", db_path);
    let mut out = File::create(&temp_path)
        .with_context(|| format!("Failed to create file: {}", &temp_path))?;

    let mut child = Command::new("pacman")
        .args(&["-Fl", "--machinereadable"])
        .stdout(Stdio::piped())
        .spawn()
        .context("Failed to run pacman")?;

    let child_stdout = BufReader::new(child.stdout.take().unwrap());

    let mut strings = Interner::new();
    let mut providers = Vec::new();

    for line in child_stdout.lines() {
        let line = line?;

        let mut parts = line.rsplit('\0');
        let mut pop = || parts.next().context("unexpeted end of line");

        let path = pop()?;

        if path.ends_with('/') {
            continue;
        }
        let (dir, bin) = match path.rfind('/') {
            Some(pos) => path.split_at(pos + 1),
            None => continue,
        };
        if !dir.ends_with("/bin/") {
            continue;
        }

        let _pkgver = pop()?;
        let package_name = strings.add(pop()?);
        let repo = strings.add(pop()?);

        providers.push(Provider {
            repo,
            package_name,
            dir: strings.add(bin),
            bin: strings.add(dir),
        });
    }

    let status = child.wait()?;
    ensure!(status.success(), "pacman failed: {}", status);

    providers.sort_unstable_by_key(|provider| strings.get(provider.bin));
    let bin_names: Vec<&str> = providers
        .iter()
        .map(|provider| strings.get(provider.bin))
        .dedup()
        .collect();

    let hash_state = phf::generate_hash(&bin_names);
    assert_eq!(bin_names.len(), hash_state.map.len());

    let header = Header {
        version: HEADER_VERSION,

        hash_key: hash_state.key,

        providers_len: byte_len(&providers),
        disps_len: byte_len(&hash_state.disps),
        table_len: (bin_names.len() * size_of::<Span>()).try_into().unwrap(),
        strings_len: strings.buf().len().try_into().unwrap(),
    };

    out.write_all(bytes_of(&header))?;
    out.write_all(cast_slice(&providers))?;
    out.write_all(cast_slice(&hash_state.disps))?;

    for &i in &hash_state.map {
        let bin = bin_names[i];
        let provider_span = find_providers(&providers, &strings, bin);

        out.write_all(bytes_of(&provider_span))?;
    }

    out.write_all(strings.buf())?;

    out.sync_all()?;
    drop(out);

    fs::rename(temp_path, db_path)?;

    Ok(())
}
