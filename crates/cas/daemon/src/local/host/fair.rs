//! Bounded FIFO intents and byte DRR before resource/serial admission.
use super::*;

const QUANTUM: usize = MAX_REQUEST_BYTES;
const HEADS: usize = 4;

#[derive(Clone, Copy)]
struct Request {
    id: u64,
    bytes: usize,
    ready: bool,
}

struct Image {
    queue: BudgetVec<Request, BudgetAllocator>,
    deficit: usize,
    admitted_bytes: u64,
    admitted: u64,
    canceled: u64,
    deferred: u64,
}

struct State {
    images: BudgetVec<Image, BudgetAllocator>,
    cursor: usize,
    replenish: bool,
    next_id: u64,
    granted: Option<(usize, u64)>,
    released_during_turn: bool,
}

pub(crate) struct Fair {
    state: Mutex<State>,
    admission: BudgetArc<admission::Admission>,
}

#[derive(Clone)]
pub(crate) struct Port {
    pub owner: BudgetArc<Fair>,
    pub image: usize,
}

pub(crate) struct Ticket {
    port: Port,
    id: u64,
}

pub(crate) struct Release(BudgetArc<Fair>);

pub(crate) struct Turn<'a> {
    ticket: &'a Ticket,
    committed: bool,
}

impl State {
    fn advance(&mut self) {
        self.cursor = (self.cursor + 1) % self.images.len();
        self.replenish = true;
    }

    fn choose(&mut self) -> Option<(usize, u64)> {
        if self.granted.is_some() {
            return None;
        }
        // Each payload is at most a quantum; a new visit can serve a head.
        for _ in 0..=self.images.len() {
            let image = &mut self.images[self.cursor];
            let Some(request) = image.queue.first() else {
                image.deficit = 0;
                self.advance();
                continue;
            };
            if !request.ready {
                self.advance();
                continue;
            }
            if self.replenish && image.deficit < request.bytes {
                image.deficit += QUANTUM;
            }
            self.replenish = false;
            if image.deficit >= request.bytes {
                return Some((self.cursor, request.id));
            }
            self.advance();
        }
        None
    }
}

impl Fair {
    pub fn new(
        images: usize,
        admission: BudgetArc<admission::Admission>,
        metadata: &Arc<Budget>,
    ) -> io::Result<BudgetArc<Self>> {
        if images == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut queues = reserved_vec(images, metadata)?;
        for _ in 0..images {
            queues.push(Image {
                queue: reserved_vec(HEADS, metadata)?,
                deficit: 0,
                admitted_bytes: 0,
                admitted: 0,
                canceled: 0,
                deferred: 0,
            });
        }
        BudgetArc::try_new(
            Self {
                state: Mutex::new(State {
                    images: queues,
                    cursor: 0,
                    replenish: true,
                    next_id: 1,
                    granted: None,
                    released_during_turn: false,
                }),
                admission,
            },
            metadata,
        )
    }

    fn wake_next(&self) -> io::Result<()> {
        let next = self
            .state
            .lock()
            .expect("admission scheduler poisoned")
            .choose();
        if let Some((image, _)) = next {
            self.admission.wake_frontend(image)?;
        }
        Ok(())
    }

    /// Capacity became available, including WAL space without an IO completion.
    /// Preserve a release racing a refused turn just like a request-credit drop.
    pub(super) fn resources_released(&self) -> io::Result<()> {
        let mut state = self.state.lock().expect("admission scheduler poisoned");
        if state.granted.is_some() {
            state.released_during_turn = true;
        }
        for image in &mut state.images {
            for request in &mut image.queue {
                request.ready = true;
            }
        }
        drop(state);
        self.wake_next()
    }

    pub fn report(&self) -> serde_json::Value {
        let state = self.state.lock().expect("admission scheduler poisoned");
        let images: Vec<_> = state
            .images
            .iter()
            .map(|image| {
                serde_json::json!({
                    "waiting":image.queue.len(), "blocked":image.queue.iter().filter(|r| !r.ready).count(), "deficit_bytes":image.deficit,
                    "admitted_bytes":image.admitted_bytes, "admitted":image.admitted,
                    "canceled":image.canceled, "deferred":image.deferred,
                })
            })
            .collect();
        serde_json::json!({"quantum_bytes":QUANTUM,"heads_per_image":HEADS,"images":images})
    }
}

impl Port {
    pub fn ticket(&self, kind: Kind) -> io::Result<Option<Ticket>> {
        let bytes = match kind {
            Kind::Control => return Ok(None),
            Kind::Read(bytes) | Kind::Write(bytes) => bytes.max(BLOCK_SIZE),
        };
        if bytes > QUANTUM {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut state = self
            .owner
            .state
            .lock()
            .expect("admission scheduler poisoned");
        let id = state.next_id;
        let next = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("admission ticket IDs exhausted"))?;
        let image = state
            .images
            .get_mut(self.image)
            .ok_or(io::ErrorKind::InvalidInput)?;
        if image.queue.len() == HEADS {
            return Err(io::Error::other("image admission head capacity exceeded"));
        }
        image.queue.push(Request {
            id,
            bytes,
            ready: true,
        });
        state.next_id = next;
        drop(state);
        Ok(Some(Ticket {
            port: self.clone(),
            id,
        }))
    }
}

impl Ticket {
    pub fn turn(&self) -> io::Result<Option<Turn<'_>>> {
        let mut state = self
            .port
            .owner
            .state
            .lock()
            .expect("admission scheduler poisoned");
        let request = state.images[self.port.image]
            .queue
            .iter_mut()
            .find(|request| request.id == self.id)
            .ok_or_else(|| io::Error::other("admission ticket already consumed"))?;
        request.ready = true;
        let next = state.choose();
        let selected = next == Some((self.port.image, self.id));
        if selected {
            state.granted = next;
            state.released_during_turn = false;
        }
        drop(state);
        if selected {
            Ok(Some(Turn {
                ticket: self,
                committed: false,
            }))
        } else {
            if let Some((image, _)) = next {
                self.port.owner.admission.wake_frontend(image)?;
            }
            Ok(None)
        }
    }
}

impl Turn<'_> {
    pub fn commit(mut self) -> Release {
        let ticket = self.ticket;
        let mut state = ticket
            .port
            .owner
            .state
            .lock()
            .expect("admission scheduler poisoned");
        assert_eq!(state.granted, Some((ticket.port.image, ticket.id)));
        let image = &mut state.images[ticket.port.image];
        let head = image.queue.remove(0);
        assert_eq!(head.id, ticket.id);
        image.deficit -= head.bytes;
        image.admitted_bytes += head.bytes as u64;
        image.admitted += 1;
        self.committed = true;
        Release(ticket.port.owner.clone())
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let ticket = self.ticket;
        let mut state = ticket
            .port
            .owner
            .state
            .lock()
            .expect("admission scheduler poisoned");
        assert_eq!(state.granted.take(), Some((ticket.port.image, ticket.id)));
        if !self.committed {
            let retry = state.released_during_turn;
            let image = &mut state.images[ticket.port.image];
            image.deferred += 1;
            // A release after reservation failed must survive this refusal.
            image.queue.first_mut().expect("granted head").ready = retry;
            state.advance();
        }
        drop(state);
        let _ = ticket.port.owner.wake_next();
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self
            .port
            .owner
            .state
            .lock()
            .expect("admission scheduler poisoned");
        let image = &mut state.images[self.port.image];
        if let Some(index) = image.queue.iter().position(|request| request.id == self.id) {
            image.queue.remove(index);
            image.canceled += 1;
        }
        drop(state);
        let _ = self.port.owner.wake_next();
    }
}

#[cfg(test)]
mod tests;

impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.resources_released();
    }
}
