//! Stop-the-world collection: every durable epoch fence precedes any sweep.
use super::{Client, Flow, Metrics};
use std::{io, net::SocketAddr, sync::Arc};
pub(crate) const STATUS: u8 = 3;
pub(crate) const FREEZE: u8 = 10;
pub(crate) const LIVE_READ: u8 = 11;
pub(crate) const LIVE_ADD: u8 = 12;
pub(crate) const SWEEP: u8 = 13;
pub(crate) const RELEASE: u8 = 14;
const BATCH: usize = 128;
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
/// Addresses are ordered by member ID. A retry resumes an unfinished common
/// epoch; participants remain fenced across restart until all have swept.
pub fn collect(
    addresses: impl AsRef<[SocketAddr]>,
    metrics: Arc<Metrics>,
) -> io::Result<Vec<serde_json::Value>> {
    let addresses = addresses.as_ref();
    if !(2..=255).contains(&addresses.len())
        || addresses
            .iter()
            .enumerate()
            .any(|(i, a)| addresses[..i].contains(a))
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match collect_once(addresses, metrics.clone()) {
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock && std::time::Instant::now() < until =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            result => return result,
        }
    }
}
fn collect_once(
    addresses: &[SocketAddr],
    metrics: Arc<Metrics>,
) -> io::Result<Vec<serde_json::Value>> {
    let mut clients = addresses
        .iter()
        .map(|a| Client::connect(*a, Flow::Collection, metrics.clone()))
        .collect::<io::Result<Vec<_>>>()?;
    let mut statuses = vec![[0; 40]; addresses.len()];
    for (client, status) in clients.iter_mut().zip(&mut statuses) {
        if client.exchange(STATUS, &mut [], status)? != 40 {
            return Err(invalid());
        }
    }
    for (node, status) in statuses.iter().enumerate() {
        if usize::from(status[0]) != node
            || status[1] != statuses[0][1]
            || usize::from(status[4]) != addresses.len()
            || status[24..] != statuses[0][24..]
        {
            return Err(invalid());
        }
    }
    let id = |s: &[u8; 40]| u64::from_le_bytes(s[8..16].try_into().unwrap());
    let active = statuses.iter().find(|s| s[2] != 0);
    let epoch = match active {
        Some(s) => id(s),
        None => statuses
            .iter()
            .map(id)
            .max()
            .unwrap()
            .checked_add(1)
            .ok_or_else(invalid)?,
    };
    if statuses.iter().any(|s| s[2] != 0 && id(s) != epoch) {
        return Err(invalid());
    }
    if active.is_some() && statuses.iter().all(|s| id(s) == epoch && s[3] != 0) {
        for client in &mut clients {
            client.exchange(RELEASE, &mut epoch.to_le_bytes(), &mut [])?;
        }
        return Ok(vec![
            serde_json::json!({"epoch":epoch,"resumed_release":true});
            addresses.len()
        ]);
    }
    let mut roots = vec![[0; 40]; addresses.len()];
    for (client, root) in clients.iter_mut().zip(&mut roots) {
        if client.exchange(FREEZE, &mut epoch.to_le_bytes(), root)? != 40 {
            return Err(invalid());
        }
    }
    // Read and verify every participant's immutable sorted live set before
    // distributing the common union. No sweep can precede this complete cut.
    let mut union = Vec::<[u8; 32]>::new();
    for (client, root) in clients.iter_mut().zip(&roots) {
        let count = u64::from_le_bytes(root[..8].try_into().unwrap());
        let mut offset = 0u64;
        let mut digest = blake3::Hasher::new();
        let mut previous = None;
        loop {
            let mut query = [0; 16];
            query[..8].copy_from_slice(&epoch.to_le_bytes());
            query[8..].copy_from_slice(&offset.to_le_bytes());
            let mut frame = [0; BATCH * 32];
            let n = client.exchange(LIVE_READ, &mut query, &mut frame)?;
            if n % 32 != 0 || n == 0 && offset != count || offset + n as u64 / 32 > count {
                return Err(invalid());
            }
            digest.update(&frame[..n]);
            for hash in frame[..n].as_chunks::<32>().0 {
                if previous.is_some_and(|p| p >= *hash) {
                    return Err(invalid());
                }
                union.push(*hash);
                previous = Some(*hash);
            }
            offset += n as u64 / 32;
            if offset == count {
                break;
            }
        }
        if digest.finalize().as_bytes() != &root[8..] {
            return Err(invalid());
        }
    }
    union.sort_unstable();
    union.dedup();
    let mut digest = blake3::Hasher::new();
    for hash in &union {
        digest.update(hash);
    }
    let mut root = [0; 40];
    root[..8].copy_from_slice(&(union.len() as u64).to_le_bytes());
    root[8..].copy_from_slice(digest.finalize().as_bytes());
    for client in &mut clients {
        let mut offset = 0;
        loop {
            let end = (offset + BATCH).min(union.len());
            let mut frame = [0; 56 + BATCH * 32];
            frame[..8].copy_from_slice(&epoch.to_le_bytes());
            frame[8..48].copy_from_slice(&root);
            frame[48..56].copy_from_slice(&(offset as u64).to_le_bytes());
            for (i, hash) in union[offset..end].iter().enumerate() {
                frame[56 + i * 32..56 + (i + 1) * 32].copy_from_slice(hash);
            }
            if client.exchange(LIVE_ADD, &mut frame[..56 + (end - offset) * 32], &mut [])? != 0 {
                return Err(invalid());
            }
            offset = end;
            if offset == union.len() {
                break;
            }
        }
    }
    let mut reports = Vec::new();
    for client in &mut clients {
        let mut bytes = [0; 4096];
        let n = client.exchange(SWEEP, &mut epoch.to_le_bytes(), &mut bytes)?;
        reports.push(serde_json::from_slice(&bytes[..n])?);
    }
    for client in &mut clients {
        client.exchange(RELEASE, &mut epoch.to_le_bytes(), &mut [])?;
    }
    Ok(reports)
}
