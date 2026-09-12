use super::*;
use crate::{BLOCK_SIZE, budget::Amount};

fn metadata() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 1024 * 1024,
        requests: 0,
    })
}

#[test]
fn continuously_ready_work_receives_one_background_and_three_demand_opportunities() {
    let metadata = metadata();
    let owner = Scheduler::new(2, &metadata).unwrap();
    {
        let mut state = owner.state.lock().unwrap();
        for turn in 0..400 {
            state.background_ready = true;
            for image in &mut state.images {
                image.ready = true;
            }
            let selected = state.selected().unwrap();
            assert_eq!(selected == Selected::Background, turn % 4 == 0);
            state.grant(selected, BLOCK_SIZE);
        }
        assert_eq!(state.counters.background, 100);
        assert_eq!(state.counters.demand, 300);
        assert_eq!(state.counters.borrowed_demand, 0);
        assert_eq!(state.counters.borrowed_background, 0);
    }
    drop(owner);
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn absent_classes_lend_opportunities_and_scope_restores_unscheduled_reference_io() {
    let metadata = metadata();
    let owner = Scheduler::new(1, &metadata).unwrap();
    let port = Scheduler::port(&owner, 0).unwrap();
    port.ready(true).unwrap();
    assert!(port.take(BLOCK_SIZE, false).unwrap());
    assert_eq!(owner.counters().borrowed_demand, 1);
    {
        let _scope = Scheduler::background(&owner);
        for _ in 0..3 {
            before_background_io(BLOCK_SIZE).unwrap();
        }
    }
    before_background_io(BLOCK_SIZE).unwrap();
    assert_eq!(owner.counters().background, 3);
    assert_eq!(owner.counters().borrowed_background, 3);
    drop((port, owner));
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn waiting_background_is_released_after_three_ready_demand_submissions() {
    let metadata = metadata();
    let owner = Scheduler::new(1, &metadata).unwrap();
    let port = Scheduler::port(&owner, 0).unwrap();
    owner.state.lock().unwrap().opportunity = 1;
    port.ready(true).unwrap();
    let copy = owner.clone();
    let worker = std::thread::spawn(move || copy.wait_background(BLOCK_SIZE).unwrap());
    let deadline = Instant::now() + Duration::from_secs(3);
    while !owner.state.lock().unwrap().background_ready {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    for _ in 0..3 {
        assert!(port.take(BLOCK_SIZE, true).unwrap());
    }
    worker.join().unwrap();
    assert_eq!(owner.counters().background, 1);
    assert_eq!(owner.counters().demand, 3);
    port.ready(false).unwrap();
    drop((port, owner));
    assert_eq!(metadata.usage().current, Amount::default());
}
