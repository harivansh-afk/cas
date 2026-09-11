//! Fixed persistence schedules for the unsynchronized sector suffix.
use super::*;

#[derive(Serialize)]
pub(super) struct Schedule {
    pub id: String,
    // Each entry is a tail-relative sector that reaches storage before the cut.
    pub persisted_sectors: Vec<usize>,
    pub tail_length: usize,
}

pub(super) fn schedules(sectors: usize, headers: &[usize]) -> Vec<Schedule> {
    if sectors == 0 {
        return vec![Schedule {
            id: "after-successful-sync".into(),
            persisted_sectors: vec![],
            tail_length: 0,
        }];
    }
    let mut schedules = Vec::new();
    for cut in 0..=sectors {
        schedules.push(Schedule {
            id: format!("truncate-{cut:03}"),
            persisted_sectors: (0..cut).collect(),
            tail_length: cut * SECTOR_BYTES,
        });
        schedules.push(Schedule {
            id: format!("reverse-{cut:03}"),
            persisted_sectors: (0..sectors).rev().take(cut).collect(),
            tail_length: sectors * SECTOR_BYTES,
        });
    }
    for cut in 0..sectors {
        schedules.push(Schedule {
            id: format!("torn-sector-{cut:03}"),
            persisted_sectors: (0..=cut).collect(),
            tail_length: cut * SECTOR_BYTES + 257,
        });
    }
    for (id, want_header) in [("headers-only", true), ("payloads-only", false)] {
        schedules.push(Schedule {
            id: id.into(),
            persisted_sectors: (0..sectors)
                .filter(|sector| headers.contains(sector) == want_header)
                .collect(),
            tail_length: sectors * SECTOR_BYTES,
        });
    }
    let mut state = SEED;
    for trial in 0..32 {
        let mut order: Vec<_> = (0..sectors).collect();
        for index in (1..sectors).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            order.swap(index, state as usize % (index + 1));
        }
        order.truncate(trial * sectors / 31);
        schedules.push(Schedule {
            id: format!("shuffle-{trial:02}"),
            persisted_sectors: order,
            tail_length: sectors * SECTOR_BYTES,
        });
    }
    schedules
}
