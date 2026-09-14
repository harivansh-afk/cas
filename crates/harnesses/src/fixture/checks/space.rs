use super::*;

pub(super) fn verify(guest: &Path) -> io::Result<()> {
    let conditions: Value = evidence::read_json(&guest.join("space-conditions.json"))?;
    let observations: Value = evidence::read_json(&guest.join("space-observations.json"))?;
    check(&conditions, &observations)
}

fn check(conditions: &Value, observations: &Value) -> io::Result<()> {
    let mib = 1024 * 1024;
    let domain = &conditions["initial"]["domain"];
    let capacity = u64_at(domain, "/capacity")?;
    let unit = u64_at(domain, "/unit")?;
    u64_at(domain, "/device")?;
    u64_at(domain, "/filesystem")?;
    require(
        unit > 0 && capacity > 36 * mib,
        "invalid physical allocation domain",
    )?;
    require(
        conditions["output_bytes_each"] == 4 * mib
            && conditions["foreground_promise"] == 20 * mib
            && conditions["background_promise"] == 16 * mib
            && conditions["limits"]["reserve"] == 36 * mib
            && conditions["limits"]["capacity"] == capacity,
        "physical fixture conditions differ",
    )?;
    require(
        observations["initial"] == conditions["initial"]
            && observations["limits"] == conditions["limits"]
            && observations["output_bytes_each"] == conditions["output_bytes_each"]
            && observations["injected_error"] == "ENOSPC after synced output",
        "physical output differs from predeclared conditions",
    )?;
    let mut allocated = Vec::new();
    for phase in ["initial", "allocated", "partial", "unlinked"] {
        let sample = &observations[phase];
        let bytes = u64_at(sample, "/allocated")?;
        require(
            sample["domain"] == *domain && bytes <= capacity && bytes.is_multiple_of(unit),
            "physical observation changed domain or exceeded capacity",
        )?;
        allocated.push(bytes);
    }
    require(
        allocated[1].saturating_sub(allocated[0]) >= 4 * mib
            && allocated[2].saturating_sub(allocated[1]) >= 4 * mib
            && allocated[3].saturating_sub(allocated[0]) >= 4 * mib,
        "preallocated or failed output disappeared from the physical account",
    )?;
    let status = &observations["status"];
    require(
        status["allocated"] == allocated[3]
            && status["promised"] == 0
            && status["failed"] == false
            && status["background_active"] == false
            && u64_at(status, "/peak_used")? >= allocated[2],
        "physical account did not reconcile completed transactions",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_measurements_changed_domain_and_lost_partial_output_fail() {
        let mib = 1024 * 1024;
        let sample = |allocated| {
            serde_json::json!({
                "domain": {"device":1, "filesystem":2, "capacity":256 * mib, "unit":4096},
                "allocated":allocated,
            })
        };
        let conditions = serde_json::json!({ "initial":sample(8 * mib),
            "limits":{"capacity":256 * mib, "reserve":36 * mib},
            "output_bytes_each":4 * mib, "foreground_promise":20 * mib,
            "background_promise":16 * mib,
        });
        let good = serde_json::json!({ "initial":conditions["initial"],
            "limits":conditions["limits"], "output_bytes_each":4 * mib,
            "injected_error":"ENOSPC after synced output", "allocated":sample(12 * mib),
            "partial":sample(16 * mib), "unlinked":sample(12 * mib),
            "status":{"allocated":12 * mib, "promised":0, "failed":false,
                "background_active":false, "peak_used":36 * mib},
        });
        check(&conditions, &good).unwrap();
        for (field, replacement) in [
            ("partial", sample(12 * mib)),
            ("unlinked", sample(8 * mib)),
            ("allocated", Value::Null),
        ] {
            let mut invalid = good.clone();
            invalid[field] = replacement;
            assert!(check(&conditions, &invalid).is_err());
        }
        let mut invalid = good.clone();
        invalid["partial"]["domain"]["device"] = Value::from(3);
        assert!(check(&conditions, &invalid).is_err());
        let mut invalid = good;
        invalid["status"]["promised"] = Value::from(1);
        assert!(check(&conditions, &invalid).is_err());
    }
}
