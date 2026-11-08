//! Stop-the-world baseline: both durable epoch fences precede either sweep.
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
/// A retry resumes an unfinished common epoch. Failed participants remain
/// fenced, including across daemon restart; there is no unilateral timeout thaw.
pub fn collect(
    addresses: [SocketAddr; 2],
    metrics: Arc<Metrics>,
) -> io::Result<[serde_json::Value; 2]> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match collect_once(addresses, metrics.clone()) {
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock && std::time::Instant::now() < until =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
}
fn collect_once(
    addresses: [SocketAddr; 2],
    metrics: Arc<Metrics>,
) -> io::Result<[serde_json::Value; 2]> {
    let mut a = Client::connect(addresses[0], Flow::Collection, metrics.clone())?;
    let mut b = Client::connect(addresses[1], Flow::Collection, metrics)?;
    let mut statuses = [[0; 40]; 2];
    for (client, status) in [&mut a, &mut b].into_iter().zip(&mut statuses) {
        if client.exchange(STATUS, &mut [], status)? != 40 {
            return Err(invalid());
        }
    }
    if statuses[0][0] != 0
        || statuses[1][0] != 1
        || statuses[0][1] != statuses[1][1]
        || statuses[0][24..] != statuses[1][24..]
    {
        return Err(invalid());
    }
    let ids = statuses.map(|s| u64::from_le_bytes(s[8..16].try_into().unwrap()));
    let active = [statuses[0][2] != 0, statuses[1][2] != 0];
    let epoch = if active[0] || active[1] {
        let current = if active[0] { ids[0] } else { ids[1] };
        if (active[0] && ids[0] != current) || (active[1] && ids[1] != current) {
            return Err(invalid());
        }
        current
    } else {
        ids[0].max(ids[1]).checked_add(1).ok_or_else(invalid)?
    };
    if active.iter().any(|v| *v)
        && ids[0] == epoch
        && ids[1] == epoch
        && statuses.iter().all(|s| s[3] != 0)
    {
        for client in [&mut a, &mut b] {
            client.exchange(RELEASE, &mut epoch.to_le_bytes(), &mut [])?;
        }
        return Ok(std::array::from_fn(
            |_| serde_json::json!({"epoch":epoch,"resumed_release":true}),
        ));
    }
    let mut roots = [[0; 40]; 2];
    for (client, root) in [&mut a, &mut b].into_iter().zip(&mut roots) {
        if client.exchange(FREEZE, &mut epoch.to_le_bytes(), root)? != 40 {
            return Err(invalid());
        }
    }
    let exchange = |source: &mut Client, target: &mut Client, root: [u8; 40]| -> io::Result<()> {
        let count = u64::from_le_bytes(root[..8].try_into().unwrap());
        let mut offset = 0u64;
        loop {
            let mut query = [0; 16];
            query[..8].copy_from_slice(&epoch.to_le_bytes());
            query[8..].copy_from_slice(&offset.to_le_bytes());
            let mut frame = [0; 56 + BATCH * 32];
            frame[..8].copy_from_slice(&epoch.to_le_bytes());
            frame[8..48].copy_from_slice(&root);
            frame[48..56].copy_from_slice(&offset.to_le_bytes());
            let n = source.exchange(LIVE_READ, &mut query, &mut frame[56..])?;
            if n % 32 != 0 || n == 0 && offset != count || offset + n as u64 / 32 > count {
                return Err(invalid());
            }
            if target.exchange(LIVE_ADD, &mut frame[..56 + n], &mut [])? != 0 {
                return Err(invalid());
            }
            offset += n as u64 / 32;
            if offset == count {
                break;
            }
        }
        Ok(())
    };
    exchange(&mut a, &mut b, roots[0])?;
    exchange(&mut b, &mut a, roots[1])?;
    let mut reports = [serde_json::Value::Null, serde_json::Value::Null];
    for (client, report) in [&mut a, &mut b].into_iter().zip(&mut reports) {
        let mut bytes = [0; 4096];
        let n = client.exchange(SWEEP, &mut epoch.to_le_bytes(), &mut bytes)?;
        *report = serde_json::from_slice(&bytes[..n])?;
    }
    // Each receiver independently confirms that the peer durably reached swept.
    // A lost release ACK is harmless: the terminal epoch record is retained.
    for client in [&mut a, &mut b] {
        client.exchange(RELEASE, &mut epoch.to_le_bytes(), &mut [])?;
    }
    Ok(reports)
}
