//! Share unfinished work, not completed results. The last cancelled waiter
//! drops the future, including any network concurrency permit it owns.
use futures_util::future::{BoxFuture, FutureExt, Shared};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

struct Flight<T> {
    generation: u64,
    waiters: usize,
    future: Shared<BoxFuture<'static, T>>,
}

pub(crate) struct SingleFlight<T> {
    flights: Mutex<HashMap<String, Flight<T>>>,
    sequence: AtomicU64,
}

impl<T> Default for SingleFlight<T> {
    fn default() -> Self {
        Self {
            flights: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(1),
        }
    }
}

struct Waiter<'a, T> {
    flights: &'a Mutex<HashMap<String, Flight<T>>>,
    key: String,
    generation: u64,
}

impl<T> Drop for Waiter<'_, T> {
    fn drop(&mut self) {
        let mut flights = self.flights.lock();
        if let Some(flight) = flights.get_mut(&self.key)
            && flight.generation == self.generation
        {
            flight.waiters -= 1;
            if flight.waiters == 0 {
                flights.remove(&self.key);
            }
        }
    }
}

impl<T: Clone + Send + Sync + 'static> SingleFlight<T> {
    pub(crate) async fn run<F, Fut>(&self, key: String, work: F) -> T
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let (future, generation) = {
            let mut flights = self.flights.lock();
            if flights
                .get(&key)
                .is_some_and(|flight| flight.future.peek().is_some())
            {
                flights.remove(&key);
            }
            let flight = flights.entry(key.clone()).or_insert_with(|| Flight {
                generation: self.sequence.fetch_add(1, Ordering::Relaxed),
                waiters: 0,
                future: async move { work().await }.boxed().shared(),
            });
            flight.waiters += 1;
            (flight.future.clone(), flight.generation)
        };
        let _waiter = Waiter {
            flights: &self.flights,
            key: key.clone(),
            generation,
        };
        let result = future.await;
        let mut flights = self.flights.lock();
        if flights
            .get(&key)
            .is_some_and(|flight| flight.generation == generation)
        {
            flights.remove(&key);
        }
        result
    }
}

#[cfg(test)]
mod tests;
