use super::*;

pub(super) fn verify(guest: &Path) -> io::Result<()> {
    let conditions = evidence::read_json(&guest.join("host-collection-conditions.json"))?;
    let observations = evidence::read_json(&guest.join("host-collection-observations.json"))?;
    check(&conditions, &observations)?;
    check_exhaustion(&evidence::read_json(
        &guest.join("unique-live-capacity.json"),
    )?)
}

fn check(conditions: &Value, observations: &Value) -> io::Result<()> {
    let mib = 1024 * 1024;
    let before = u64_at(conditions, "/initial/allocated")?;
    let capacity = u64_at(conditions, "/limits/capacity")?;
    let reserve = u64_at(conditions, "/limits/reserve")?;
    require(
        conditions["payload_bytes"] == 96 * mib
            && conditions["headroom_bytes"] == 8 * mib
            && conditions["denied_promise_bytes"] == 16 * mib
            && capacity
                .checked_sub(reserve)
                .and_then(|n| n.checked_sub(before))
                == Some(8 * mib),
        "host collection conditions differ",
    )?;
    require(
        observations["conditions"] == *conditions,
        "collection changed its declared conditions",
    )?;
    let after = &observations["after"];
    let allocated = u64_at(after, "/allocated")?;
    require(
        before
            .checked_sub(allocated)
            .is_some_and(|freed| freed >= 96 * mib)
            && u128::from(allocated) * 100 < u128::from(capacity - reserve) * 60
            && after["promised"] == 0
            && after["failed"] == false
            && after["pressured"] == false
            && after["background_active"] == false,
        "collection did not physically reclaim and resume admission",
    )?;
    let host = &observations["host"];
    let last = &host["collection"]["last"];
    require(
        u64_at(host, "/collection/completed")? > 0
            && host["collection"]["error"].is_null()
            && host["failure"].is_null()
            && host["admission"]["failed"] == false
            && host["admission"]["paused"] == false
            && last["capacity_exhausted"] == false
            && u64_at(last, "/rounds")? > 0
            && u64_at(last, "/chunks/segments_removed")? > 0
            && u64_at(last, "/pause_micros")? > 0
            && observations["admitted_after"] == 1
            && observations["read_oracle"] == true,
        "collection did not prove automatic progress and the resumed read oracle",
    )
}

fn check_exhaustion(value: &Value) -> io::Result<()> {
    let mib = 1024 * 1024;
    let initial = u64_at(value, "/initial/allocated")?;
    let capacity = u64_at(value, "/limits/capacity")?;
    let reserve = u64_at(value, "/limits/reserve")?;
    let after = &value["after"];
    let allocated = u64_at(after, "/allocated")?;
    require(
        value["live_bytes"] == 256 * mib
            && value["read_bytes"] == value["live_bytes"]
            && value["admitted_mutations"] == 0
            && u64_at(value, "/seed_micros")? > 0
            && u64_at(value, "/read_micros")? > 0
            && reserve == 150 * mib
            && initial.checked_add(reserve) == Some(capacity)
            && capacity <= u64_at(value, "/initial/domain/capacity")?
            && capacity
                .checked_sub(reserve)
                .is_some_and(|limit| allocated <= limit)
            && u128::from(allocated) * 100 >= u128::from(capacity) * 60
            && u64_at(after, "/peak_used")? <= capacity
            && after["promised"] == 0
            && after["failed"] == false
            && after["pressured"] == true
            && after["background_active"] == false,
        "unique live capacity or read accounting differs",
    )?;
    let host = &value["host"];
    let collection = &host["collection"];
    require(
        host["failure"].is_null()
            && host["admission"]["failed"] == false
            && host["store"]["chunks"] == 65536
            && host["store"]["failed"] == false
            && u64_at(collection, "/completed")? > 0
            && collection["error"].is_null()
            && collection["last"]["capacity_exhausted"] == true
            && u64_at(collection, "/last/rounds")? > 0,
        "unique live collection did not establish healthy capacity exhaustion",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustion_requires_full_live_reads_intact_reserve_and_closed_admission() {
        let mib: u64 = 1024 * 1024;
        let good = serde_json::json!({
            "live_bytes":256*mib, "read_bytes":256*mib, "admitted_mutations":0,
            "seed_micros":1, "read_micros":1,
            "initial":{"allocated":400*mib,"domain":{"capacity":2048*mib}},
            "limits":{"capacity":550*mib,"reserve":150*mib},
            "after":{"allocated":350*mib,"peak_used":417*mib,"promised":0,
                "failed":false,"pressured":true,"background_active":false},
            "host":{"failure":null,"admission":{"failed":false},
                "store":{"chunks":65536,"failed":false},
                "collection":{"completed":1,"error":null,
                    "last":{"capacity_exhausted":true,"rounds":1}}}
        });
        check_exhaustion(&good).unwrap();
        for field in [
            "read_bytes",
            "admitted_mutations",
            "initial",
            "limits",
            "after",
            "host",
        ] {
            let mut bad = good.clone();
            bad[field] = Value::Null;
            assert!(check_exhaustion(&bad).is_err());
        }
        for (pointer, value) in [
            ("/read_bytes", Value::from(255 * mib)),
            ("/admitted_mutations", Value::from(1)),
            ("/after/allocated", Value::from(401 * mib)),
            ("/after/allocated", Value::from(329 * mib)),
            ("/after/promised", Value::from(4096)),
            ("/after/pressured", Value::Bool(false)),
            ("/host/store/chunks", Value::from(65535)),
            (
                "/host/collection/last/capacity_exhausted",
                Value::Bool(false),
            ),
        ] {
            let mut bad = good.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(check_exhaustion(&bad).is_err(), "{pointer}");
        }
    }

    #[test]
    fn missing_reclamation_retained_promises_or_failed_oracles_reject_progress() {
        let mib = 1024 * 1024;
        let conditions = serde_json::json!({"payload_bytes":96*mib,
            "initial":{"allocated":160*mib}, "limits":{"capacity":318*mib,"reserve":150*mib},
            "headroom_bytes":8*mib,"denied_promise_bytes":16*mib});
        let good = serde_json::json!({"conditions":conditions,
            "after":{"allocated":64*mib,"promised":0,"failed":false,"pressured":false,"background_active":false},
            "host":{"failure":null,"admission":{"failed":false,"paused":false},
                "collection":{"completed":1,"error":null,"last":{"capacity_exhausted":false,
                    "rounds":1,"chunks":{"segments_removed":48},"pause_micros":10}}},
            "admitted_after":1,"read_oracle":true});
        check(&conditions, &good).unwrap();
        for field in [
            "conditions",
            "after",
            "host",
            "admitted_after",
            "read_oracle",
        ] {
            let mut bad = good.clone();
            bad[field] = Value::Null;
            assert!(check(&conditions, &bad).is_err());
        }
        for (field, value) in [
            ("allocated", Value::from(100 * mib)),
            ("promised", Value::from(4096)),
            ("pressured", Value::Bool(true)),
        ] {
            let mut bad = good.clone();
            bad["after"][field] = value;
            assert!(check(&conditions, &bad).is_err());
        }
    }
}
