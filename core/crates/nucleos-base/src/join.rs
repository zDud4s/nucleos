//! Awaiting a set of futures concurrently, which is the one thing this crate needs from `futures`.
//!
//! Hand-written rather than taken from the `futures` crate, and `tokio::spawn` per future is the
//! other obvious shape and worse: spawning demands `'static`, so every caller would have to clone
//! whatever the futures borrow — for `council.rs` that meant a whole `Driver` per seat.
//!
//! **It exists as a module because it existed twice.** `council.rs::futures_join_all` and
//! `assistants.rs::join_all_concurrently` were the same twenty lines under two names, and the
//! second one's own doc said so while adding itself anyway: the packet that wrote it was not
//! allowed to edit `council.rs`, so the only move available to it was the duplication it was
//! documenting. Neither copy could be the shared one without an import pointing the wrong way —
//! an assistant factory has no business depending on the council — so the function moved out from
//! under both.
//!
//! Owns how a set of futures is driven and nothing else: it does not spawn, does not time out, and
//! does not decide what happens to a result. A caller that needs a deadline wraps its own futures
//! before handing them over.

/// Awaits every future concurrently and returns their outputs **in the order the futures went in**,
/// not the order they finished.
///
/// The ordering is the reason for the `Vec<Option<_>>`: results are parked at the index their
/// future came from, so a slow future cannot push a fast one out of place. `declared_for` and the
/// council's seats both index straight back into the input, and neither would survive results
/// arriving in completion order.
pub async fn all<F: std::future::Future>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    let mut pending: Vec<std::pin::Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut results: Vec<Option<F::Output>> = (0..pending.len()).map(|_| None).collect();
    let mut remaining = pending.len();

    std::future::poll_fn(|context| {
        for (index, future) in pending.iter_mut().enumerate() {
            if results[index].is_some() {
                continue;
            }
            if let std::task::Poll::Ready(output) = future.as_mut().poll(context) {
                results[index] = Some(output);
                remaining -= 1;
            }
        }
        if remaining == 0 {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;

    results
        .into_iter()
        .map(|result| result.expect("every future resolved before the join returned"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::all;

    /// The futures here finish in a deliberately scrambled order — the third one takes the most
    /// turns to settle and the second none at all — so a join that returned completion order would
    /// come back `1, 3, 0, 2` and this would catch it.
    #[tokio::test]
    async fn the_results_come_back_in_the_order_the_futures_went_in() {
        let turns = [3usize, 0, 5, 1];
        let futures = turns.iter().enumerate().map(|(index, &turns)| async move {
            for _ in 0..turns {
                tokio::task::yield_now().await;
            }
            index
        });

        assert_eq!(all(futures).await, vec![0, 1, 2, 3]);
    }

    /// Concurrency, asserted without a clock. Every future bumps the counter before it yields, so
    /// each one reads the total only after all four have started. Driving them one after another
    /// would read `1, 2, 3, 4` instead — which is what this is here to refuse.
    #[tokio::test]
    async fn every_future_starts_before_any_of_them_finishes() {
        let started = std::cell::Cell::new(0usize);
        let futures = (0..4).map(|_| async {
            started.set(started.get() + 1);
            tokio::task::yield_now().await;
            started.get()
        });

        assert_eq!(all(futures).await, vec![4, 4, 4, 4]);
    }

    /// The empty case, because `remaining == 0` is true on the first poll and the loop body never
    /// runs — the one path where `poll_fn` returns `Ready` without having polled anything.
    #[tokio::test]
    async fn nothing_to_await_returns_nothing() {
        let futures: Vec<std::future::Ready<u8>> = Vec::new();
        assert_eq!(all(futures).await, Vec::<u8>::new());
    }
}
