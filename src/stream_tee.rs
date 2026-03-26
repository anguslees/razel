use futures::stream::Stream;
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

pub trait StreamTeeExt: Stream + Sized {
    /// Wraps this stream in a Tee, returning the first consumer handle.
    /// Additional consumers can be created by calling `.clone()` on the returned `StreamTee`.
    fn tee(self) -> StreamTee<Self>;
}

impl<S: Stream> StreamTeeExt for S {
    fn tee(self) -> StreamTee<Self> {
        let state = SharedState {
            stream: Box::pin(self),
            buffer: VecDeque::new(),
            next_id: 1,
            buffer_start_index: 0,
            cursors: HashMap::new(),
            wakers: HashMap::new(),
            stream_done: false,
        };

        let shared = Arc::new(Mutex::new(state));
        shared.lock().expect("Mutex poisoned").cursors.insert(0, 0);

        StreamTee { id: 0, shared }
    }
}

type ConsumerId = usize;

struct SharedState<S: Stream> {
    stream: Pin<Box<S>>,
    buffer: VecDeque<S::Item>,
    next_id: ConsumerId,
    buffer_start_index: usize,
    cursors: HashMap<ConsumerId, usize>,
    wakers: HashMap<ConsumerId, Waker>,
    stream_done: bool,
}

impl<S: Stream> SharedState<S> {
    fn gc(&mut self) {
        if self.cursors.is_empty() {
            // If there are no consumers left, we can clear the entire buffer
            self.buffer.clear();
            self.buffer_start_index += self.buffer.len(); // just keep it mathematically correct
            return;
        }

        // Find the minimum cursor across all active consumers.
        let min_cursor = *self
            .cursors
            .values()
            .min()
            .expect("at least one cursor to exist for GC");

        // Remove elements from the front of the deque until its starting index matches min_cursor
        while self.buffer_start_index < min_cursor && !self.buffer.is_empty() {
            self.buffer.pop_front();
            self.buffer_start_index += 1;
        }
    }
}

pub struct StreamTee<S: Stream> {
    id: ConsumerId,
    shared: Arc<Mutex<SharedState<S>>>,
}

impl<S: Stream> Clone for StreamTee<S> {
    fn clone(&self) -> Self {
        let mut state = self.shared.lock().expect("Mutex poisoned");
        let new_id = state.next_id;
        state.next_id += 1;

        // Use the cloner's current position.  This could lose items, if the cloner has already consumed some of them.
        let current_cursor = *state
            .cursors
            .get(&self.id)
            .unwrap_or(&state.buffer_start_index);
        state.cursors.insert(new_id, current_cursor);

        Self {
            id: new_id,
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<S: Stream> Drop for StreamTee<S> {
    fn drop(&mut self) {
        let mut state = self.shared.lock().expect("Mutex poisoned");
        state.cursors.remove(&self.id);
        state.wakers.remove(&self.id);
        state.gc();

        // If we drop a consumer, it might have been the one whose Waker is currently
        // registered with the underlying stream. To avoid stalling the remaining consumers
        // that are waiting for new items, we wake them all up. They will re-poll and one
        // of them will re-register its Waker with the underlying stream.
        for (_, waker) in state.wakers.drain() {
            waker.wake();
        }
    }
}

impl<S: Stream> Stream for StreamTee<S>
where
    S::Item: Clone,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut state = self.shared.lock().expect("Mutex poisoned");
        let cursor_val = *state.cursors.get(&self.id).expect("cursor must exist");
        let relative_index = cursor_val - state.buffer_start_index;

        if relative_index < state.buffer.len() {
            // The item is already in the buffer!
            let item = state.buffer[relative_index].clone();

            // Advance cursor and GC
            *state.cursors.get_mut(&self.id).expect("cursor must exist") += 1;
            state.gc();

            return Poll::Ready(Some(item));
        }

        if state.stream_done {
            return Poll::Ready(None);
        }

        // We are at the bleeding edge. Poll the underlying stream.
        match state.stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                state.buffer.push_back(item.clone());

                // Advance our cursor
                *state.cursors.get_mut(&self.id).expect("cursor must exist") += 1;

                // Wake up all other consumers that might be waiting for this item
                for (id, waker) in state.wakers.drain() {
                    // Don't wake ourselves, though removing from the map is safe anyway
                    if id != self.id {
                        waker.wake();
                    }
                }

                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                state.stream_done = true;
                // Wake everyone so they can read None
                for (id, waker) in state.wakers.drain() {
                    if id != self.id {
                        waker.wake();
                    }
                }
                Poll::Ready(None)
            }
            Poll::Pending => {
                // Register our waker
                state.wakers.insert(self.id, cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{self, StreamExt};

    #[tokio::test]
    async fn test_all_consumers_get_all_items() {
        let source = stream::iter(vec![1, 2, 3]);
        let mut t1 = source.tee();
        let mut t2 = t1.clone();
        let mut t3 = t1.clone();

        assert_eq!(t1.next().await, Some(1));
        assert_eq!(t2.next().await, Some(1));
        assert_eq!(t3.next().await, Some(1));

        assert_eq!(t2.next().await, Some(2));
        assert_eq!(t1.next().await, Some(2));
        assert_eq!(t3.next().await, Some(2));

        assert_eq!(t3.next().await, Some(3));
        assert_eq!(t2.next().await, Some(3));
        assert_eq!(t1.next().await, Some(3));

        assert_eq!(t1.next().await, None);
        assert_eq!(t2.next().await, None);
        assert_eq!(t3.next().await, None);
    }

    #[tokio::test]
    async fn test_buffer_gc() {
        let source = stream::iter(vec![1, 2, 3, 4, 5]);
        let mut t1 = source.tee();
        let mut t2 = t1.clone();

        // t1 races ahead
        assert_eq!(t1.next().await, Some(1));
        assert_eq!(t1.next().await, Some(2));
        assert_eq!(t1.next().await, Some(3));

        {
            let state = t1.shared.lock().expect("Mutex poisioned");
            assert_eq!(state.buffer.len(), 3); // items 1, 2, 3 buffered because t2 is at index 0
        }

        // t2 catches up a bit
        assert_eq!(t2.next().await, Some(1));
        assert_eq!(t2.next().await, Some(2));

        {
            let state = t1.shared.lock().expect("Mutex poisioned");
            assert_eq!(state.buffer.len(), 1); // 1 and 2 gc'd, only 3 is buffered since t2 is at index 2
        }

        // t2 catches up fully
        assert_eq!(t2.next().await, Some(3));
        {
            let state = t1.shared.lock().expect("Mutex poisioned");
            assert_eq!(state.buffer.len(), 0); // all items consumed!
        }
    }

    #[tokio::test]
    async fn test_dropping_slow_consumer_clears_buffer() {
        let source = stream::iter(vec![1, 2, 3, 4, 5]);
        let mut t1 = source.tee();
        let t2 = t1.clone();

        // t1 races ahead
        assert_eq!(t1.next().await, Some(1));
        assert_eq!(t1.next().await, Some(2));
        assert_eq!(t1.next().await, Some(3));

        {
            let state = t1.shared.lock().expect("Mutex poisioned");
            assert_eq!(state.buffer.len(), 3);
        }

        // drop t2
        drop(t2);

        {
            // Now t1 is the only consumer, and its cursor is at index 3,
            // so the entire buffer should have been cleared instantly via `Drop`.
            let state = t1.shared.lock().expect("Mutex poisioned");
            assert_eq!(state.buffer.len(), 0);
        }

        // t1 can continue
        assert_eq!(t1.next().await, Some(4));
        assert_eq!(t1.next().await, Some(5));
        assert_eq!(t1.next().await, None);
    }
}
