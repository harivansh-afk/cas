use super::*;

pub(super) fn verify(guest: &Path) -> io::Result<()> {
    let conditions = evidence::read_json(&guest.join("host-collection-conditions.json"))?;
    let observations = evidence::read_json(&guest.join("host-collection-observations.json"))?;
    check(&conditions, &observations)
}

fn number(value: &Value) -> io::Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| io::Error::other("missing host collection measurement"))
}

fn check(conditions: &Value, observations: &Value) -> io::Result<()> {
    let mib = 1024 * 1024;
    let before = number(&conditions["initial"]["allocated"])?;
    let capacity = number(&conditions["limits"]["capacity"])?;
    let reserve = number(&conditions["limits"]["reserve"])?;
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
    let allocated = number(&after["allocated"])?;
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
        number(&host["collection"]["completed"])? > 0
            && host["collection"]["error"].is_null()
            && host["failure"].is_null()
            && host["admission"]["failed"] == false
            && host["admission"]["paused"] == false
            && last["capacity_exhausted"] == false
            && number(&last["rounds"])? > 0
            && number(&last["chunks"]["segments_removed"])? > 0
            && number(&last["pause_micros"])? > 0
            && observations["admitted_after"] == 1
            && observations["read_oracle"] == true,
        "collection did not prove automatic progress and the resumed read oracle",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
