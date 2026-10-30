//! Per-origin connection-pool primitives.
//!
//! This is a deliberately close, dependency-amputated port of
//! `hyper-util` 0.1.20 `client/legacy/pool.rs`. The important distinction is
//! structural: HTTP/1 reservations are unique while HTTP/2 reservations are
//! shared. Do not collapse that distinction into response-body completion.

use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::fmt::{self, Debug};
use std::future::Future;
use std::hash::Hash;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_channel::oneshot;
use futures_util::future;
use futures_util::task::AtomicWaker;

use super::{DebugEvent, DebugEventLog};
use h12tiny_core::runtime::BoxExecutor;

/// An item that can participate in a pool. The pool knows no protocol
/// details; the client supplies the H1/H2 reservation semantics here.
pub(crate) trait Poolable: Unpin + Send + Sized + 'static {
    fn is_open(&self) -> bool;
    fn reserve(self) -> Reservation<Self>;
    fn can_share(&self) -> bool;
}

pub(crate) trait Key: Eq + Hash + Clone + Debug + Unpin + Send + 'static {
    /// Produces the endpoint identity shown by a development event. This is a
    /// pool boundary rather than a `Debug` rendering so events remain useful
    /// when a key's internal representation changes.
    fn origin(&self) -> String;
}

impl Key for super::normalize::PoolKey {
    fn origin(&self) -> String {
        super::normalize::pool_key_origin(self)
    }
}

impl Key for String {
    fn origin(&self) -> String {
        self.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    Auto,
    Http1,
    Http2,
}

/// The pool's only protocol-specific state transition.
pub(crate) enum Reservation<T> {
    #[cfg(feature = "http2")]
    Shared(T, T),
    #[cfg_attr(not(feature = "http1"), allow(dead_code))]
    Unique(T),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    pub(crate) idle_timeout: Option<Duration>,
    pub(crate) max_idle_per_host: usize,
    /// Bounds every open H1 socket for an origin, including an in-progress
    /// establishment and an idle socket retained for later reuse.
    pub(crate) max_h1_connections_per_host: usize,
}

impl Config {
    pub(crate) fn enabled(self) -> bool {
        self.max_idle_per_host > 0
    }
}

/// A pool is disabled rather than partially active when `max_idle_per_host`
/// is zero. That is the legacy client's observable contract.
pub(crate) struct Pool<T, K: Key> {
    inner: Option<Arc<Mutex<Inner<T, K>>>>,
    debug_events: Option<DebugEventLog>,
}

struct Inner<T, K: Key> {
    /// H2 has at most one establishment owner per origin. H1 reservations are
    /// counted separately because each H1 socket is unique to one exchange.
    connecting: HashSet<K>,
    h1_connections: Arc<Mutex<HashMap<K, H1Slots>>>,
    idle: HashMap<K, Vec<Idle<T, K>>>,
    max_idle_per_host: usize,
    max_h1_connections_per_host: usize,
    waiters: HashMap<K, VecDeque<oneshot::Sender<Connection<T, K>>>>,
    idle_interval_ref: Option<oneshot::Sender<Infallible>>,
    executor: BoxExecutor,
    timer: Option<Arc<dyn hyper::rt::Timer + Send + Sync>>,
    timeout: Option<Duration>,
    debug_events: Option<DebugEventLog>,
}

impl<T, K: Key> Pool<T, K> {
    pub(crate) fn new(
        config: Config,
        executor: BoxExecutor,
        timer: Option<Arc<dyn hyper::rt::Timer + Send + Sync>>,
        debug_events: Option<DebugEventLog>,
    ) -> Self {
        let inner = config.enabled().then(|| {
            Arc::new(Mutex::new(Inner {
                connecting: HashSet::new(),
                h1_connections: Arc::new(Mutex::new(HashMap::new())),
                idle: HashMap::new(),
                max_idle_per_host: config.max_idle_per_host,
                max_h1_connections_per_host: config.max_h1_connections_per_host,
                waiters: HashMap::new(),
                idle_interval_ref: None,
                executor,
                timer,
                timeout: config.idle_timeout,
                debug_events: debug_events.clone(),
            }))
        });
        Self {
            inner,
            debug_events,
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }
}

impl<T: Poolable, K: Key> Pool<T, K> {
    pub(crate) fn checkout(&self, key: K) -> Checkout<T, K> {
        if let Some(events) = &self.debug_events {
            events.record(DebugEvent::PoolCheckout {
                origin: key.origin(),
            });
        }
        Checkout {
            key,
            pool: self.clone(),
            waiter: None,
            slot_waiter: None,
            phase: CheckoutPhase::RacingDial,
        }
    }

    /// Acquires a connection-establishment reservation. H2 has one shared
    /// establishment owner per origin; H1 consumes one bounded socket slot
    /// that is retained until that connection is closed.
    ///
    /// A reservation is represented by a `Connecting` drop guard, so a
    /// cancelled establishment cannot permanently consume a slot or strand
    /// waiters.
    pub(crate) fn connecting(&self, key: &K, protocol: Protocol) -> Option<Connecting<T, K>> {
        if let Some(inner) = &self.inner {
            let mut locked = inner.lock().expect("pool mutex poisoned");
            if protocol == Protocol::Http2 {
                if !locked.connecting.insert(key.clone()) {
                    return None;
                }
                return Some(Connecting {
                    key: key.clone(),
                    pool: WeakOpt::downgrade(inner),
                    h1_slot: None,
                });
            }
            let h1_slot = locked.reserve_h1_slot(key)?;
            return Some(Connecting {
                key: key.clone(),
                pool: WeakOpt::downgrade(inner),
                h1_slot: Some(h1_slot),
            });
        }
        Some(Connecting {
            key: key.clone(),
            pool: WeakOpt::none(),
            h1_slot: None,
        })
    }

    pub(crate) fn pooled(
        &self,
        #[cfg_attr(not(feature = "http2"), allow(unused_mut))] mut connecting: Connecting<T, K>,
        value: T,
    ) -> Pooled<T, K> {
        let (value, pool, h1_slot) = match &self.inner {
            Some(inner) => match value.reserve() {
                #[cfg(feature = "http2")]
                Reservation::Shared(to_insert, to_return) => {
                    let pooled = Self::put(
                        inner,
                        connecting.key.clone(),
                        Connection { value: to_insert, h1_slot: None },
                    ).is_ok();
                    let waiters = inner.lock().expect("pool mutex poisoned").connected(&connecting.key);
                    drop(waiters);
                    if pooled {
                        if let Some(events) = &self.debug_events {
                            events.record(DebugEvent::ConnectionPooled {
                                origin: connecting.key.origin(),
                            });
                        }
                    }
                    connecting.pool = WeakOpt::none();
                    drop(connecting.h1_slot.take());
                    (to_return, WeakOpt::none(), None)
                }
                Reservation::Unique(value) => {
                    let h1_slot = connecting.h1_slot.take();
                    // The returned `Pooled` value now owns the H1 slot. Its
                    // eventual drop will either hand the socket to a waiter
                    // or release the slot; this establishment guard must not
                    // wake those waiters as though the dial had failed.
                    connecting.pool = WeakOpt::none();
                    (value, WeakOpt::downgrade(inner), h1_slot)
                }
            },
            None => (value, WeakOpt::none(), None),
        };
        Pooled {
            value: Some(Connection { value, h1_slot }),
            is_reused: false,
            key: connecting.key.clone(),
            pool,
            debug_events: self.debug_events.clone(),
        }
    }

    fn put(pool: &Arc<Mutex<Inner<T, K>>>, key: K, mut value: Connection<T, K>) -> Result<(), Connection<T, K>> {
        loop {
            let sender = {
                let mut inner = pool.lock().expect("pool mutex poisoned");
                if value.value.can_share() && inner.idle.contains_key(&key) {
                    return Err(value);
                }
                let sender = inner.waiters.get_mut(&key).and_then(VecDeque::pop_front);
                if inner.waiters.get(&key).is_some_and(VecDeque::is_empty) {
                    inner.waiters.remove(&key);
                }
                if sender.is_none() {
                    let now = inner.now();
                    let max_idle = inner.max_idle_per_host;
                    let list = inner.idle.entry(key).or_default();
                    if list.len() >= max_idle {
                        return Err(value);
                    }
                    list.push(Idle { value, idle_at: now });
                    inner.spawn_idle_interval(pool);
                    return Ok(());
                }
                sender.expect("sender was checked")
            };
            match value.value.reserve() {
                #[cfg(feature = "http2")]
                Reservation::Shared(to_keep, to_send) => {
                    value = Connection { value: to_keep, h1_slot: None };
                    if let Err(returned) = sender.send(Connection { value: to_send, h1_slot: None }) {
                        value = returned;
                    }
                }
                Reservation::Unique(to_send) => {
                    let candidate = Connection { value: to_send, h1_slot: value.h1_slot };
                    match sender.send(candidate) {
                        Ok(()) => return Ok(()),
                        Err(returned) => { value = returned; continue; }
                    }
                }
            }
        }
    }

    fn reuse(&self, key: &K, value: Connection<T, K>) -> Pooled<T, K> {
        let shared = value.value.can_share();
        let pool = if shared {
            WeakOpt::none()
        } else {
            self.inner
                .as_ref()
                .map_or_else(WeakOpt::none, WeakOpt::downgrade)
        };
        Pooled {
            value: Some(value),
            is_reused: true,
            key: key.clone(),
            pool,
            debug_events: self.debug_events.clone(),
        }
    }
}

impl<T, K: Key> Clone for Pool<T, K> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            debug_events: self.debug_events.clone(),
        }
    }
}

// Delivered connections can be cancelled independently of pool operations.
// Slot cleanup therefore uses its own registry, never the pool mutex. Pool
// operations acquire the pool mutex before this registry; slot release only
// acquires the registry and invokes wake callbacks after unlocking it.
struct H1Slots {
    count: usize,
    notification: Arc<SlotNotification>,
    waiters: Vec<Weak<AtomicWaker>>,
}

#[derive(Default)]
struct SlotNotification {
    released: AtomicUsize,
}

struct H1Slot<K: Key> {
    key: K,
    slots: Arc<Mutex<HashMap<K, H1Slots>>>,
}

impl<K: Key> Drop for H1Slot<K> {
    fn drop(&mut self) {
        let waiters = {
            let mut slots = self.slots.lock().expect("H1 slots mutex poisoned");
            let state = slots.get_mut(&self.key).expect("H1 slot was released twice");
            assert!(state.count > 0, "H1 slot was released twice");
            state.count -= 1;
            state.notification.released.fetch_add(1, Ordering::AcqRel);
            state.waiters.retain(|waiter| waiter.strong_count() > 0);
            let live = state.waiters.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
            if state.count == 0 {
                slots.remove(&self.key);
            }
            live
        };
        for waiter in waiters {
            waiter.wake();
        }
    }
}

// A checkout retries once after observing a release. A new checkout observes
// the current generation, so an exhausted cap cannot produce a wake loop.
struct SlotWaiter {
    notification: Arc<SlotNotification>,
    released: usize,
    waker: Arc<AtomicWaker>,
}

struct Connection<T, K: Key> {
    value: T,
    h1_slot: Option<H1Slot<K>>,
}

struct Idle<T, K: Key> {
    value: Connection<T, K>,
    idle_at: Instant,
}

struct IdlePopper<'a, T, K: Key> {
    _key: &'a K,
    list: &'a mut Vec<Idle<T, K>>,
}

impl<T: Poolable, K: Key> IdlePopper<'_, T, K> {
    fn pop(
        self,
        expiration: Expiration,
        now: Instant,
        debug_events: Option<&DebugEventLog>,
        discarded: &mut Vec<Connection<T, K>>,
    ) -> Option<Idle<T, K>> {
        while let Some(entry) = self.list.pop() {
            if !entry.value.value.is_open() || expiration.expires(entry.idle_at, now) {
                if let Some(events) = debug_events {
                    events.record(DebugEvent::PoolEvicted {
                        origin: self._key.origin(),
                    });
                }
                discarded.push(entry.value);
                continue;
            }
            let value = match entry.value.value.reserve() {
                #[cfg(feature = "http2")]
                Reservation::Shared(to_reinsert, to_checkout) => {
                    self.list.push(Idle {
                        idle_at: now,
                        value: Connection { value: to_reinsert, h1_slot: None },
                    });
                    to_checkout
                }
                Reservation::Unique(value) => value,
            };
            return Some(Idle {
                value: Connection { value, h1_slot: entry.value.h1_slot },
                idle_at: entry.idle_at,
            });
        }
        None
    }
}

impl<T: Poolable, K: Key> Inner<T, K> {
    fn now(&self) -> Instant {
        self.timer
            .as_ref()
            .map_or_else(Instant::now, |timer| timer.now())
    }

    fn reserve_h1_slot(&mut self, key: &K) -> Option<H1Slot<K>> {
        if self.max_h1_connections_per_host == 0 {
            return None;
        }
        let mut slots = self.h1_connections.lock().expect("H1 slots mutex poisoned");
        let state = slots.entry(key.clone()).or_insert_with(|| H1Slots {
            count: 0,
            notification: Arc::new(SlotNotification::default()),
            waiters: Vec::new(),
        });
        if state.count >= self.max_h1_connections_per_host {
            return None;
        }
        state.count += 1;
        Some(H1Slot {
            key: key.clone(),
            slots: self.h1_connections.clone(),
        })
    }

    /// Removes an establishment marker and wakes ownership waiters by
    /// dropping their senders. Their parent client futures then retry rather
    /// than remaining parked behind a cancelled connecting task.
    #[cfg_attr(not(feature = "http2"), allow(dead_code))]
    fn connected(&mut self, key: &K) -> Option<VecDeque<oneshot::Sender<Connection<T, K>>>> {
        self.connecting.remove(key);
        self.waiters.remove(key)
    }

    fn connecting_cancelled(&mut self, key: &K, h1_slot: bool) -> Option<VecDeque<oneshot::Sender<Connection<T, K>>>> {
        if !h1_slot {
            self.connecting.remove(key);
        }
        self.waiters.remove(key)
    }

    fn clean_waiters(&mut self, key: &K) {
        let remove = self
            .waiters
            .get_mut(key)
            .map(|waiters| {
                waiters.retain(|sender| !sender.is_canceled());
                waiters.is_empty()
            })
            .unwrap_or(false);
        if remove {
            self.waiters.remove(key);
        }
    }

    // Return evictions to the caller so their slot-release callbacks run
    // after the caller releases the pool mutex.
    fn clear_expired(&mut self) -> Vec<Connection<T, K>> {
        let timeout = self.timeout.expect("idle task requires a timeout");
        let now = self.now();
        let debug_events = self.debug_events.clone();
        let mut discarded = Vec::new();
        self.idle.retain(|key, entries| {
            for index in (0..entries.len()).rev() {
                let entry = &entries[index];
                if !entry.value.value.is_open()
                    || now.saturating_duration_since(entry.idle_at) > timeout
                {
                    if let Some(events) = &debug_events {
                        events.record(DebugEvent::PoolEvicted { origin: key.origin() });
                    }
                    discarded.push(entries.swap_remove(index).value);
                }
            }
            !entries.is_empty()
        });
        discarded
    }

    fn spawn_idle_interval(&mut self, pool: &Arc<Mutex<Self>>) {
        if self.idle_interval_ref.is_some() {
            return;
        }
        let Some(timeout) = self.timeout.filter(|timeout| *timeout > Duration::ZERO) else {
            return;
        };
        let Some(timer) = self.timer.clone() else {
            return;
        };
        let (sender, receiver) = oneshot::channel();
        self.idle_interval_ref = Some(sender);
        let task = IdleTask {
            timer,
            duration: timeout.max(Duration::from_millis(90)),
            pool: WeakOpt::downgrade(pool),
            pool_drop_notifier: receiver,
        };
        self.executor.execute(task.run());
    }
}

/// A checked-out connection. Dropping a unique H1 reservation returns it to
/// the pool only when Hyper reports it still usable; shared H2 values already
/// have their pool copy before being checked out.
pub(crate) struct Pooled<T: Poolable, K: Key> {
    value: Option<Connection<T, K>>,
    is_reused: bool,
    key: K,
    pool: WeakOpt<Mutex<Inner<T, K>>>,
    debug_events: Option<DebugEventLog>,
}

impl<T: Poolable, K: Key> Pooled<T, K> {
    pub(crate) fn is_reused(&self) -> bool {
        self.is_reused
    }

    pub(crate) fn is_pool_enabled(&self) -> bool {
        self.pool.0.is_some()
    }
}

impl<T: Poolable, K: Key> Deref for Pooled<T, K> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value.as_ref().expect("pooled value was taken").value
    }
}

impl<T: Poolable, K: Key> DerefMut for Pooled<T, K> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value.as_mut().expect("pooled value was taken").value
    }
}

impl<T: Poolable, K: Key> Drop for Pooled<T, K> {
    fn drop(&mut self) {
        let Some(value) = self.value.take() else {
            return;
        };
        if !value.value.is_open() {
            if let Some(events) = &self.debug_events {
                events.record(DebugEvent::ConnectionClosed {
                    origin: self.key.origin(),
                });
            }
            return;
        }
        if let Some(pool) = self.pool.upgrade() {
            let result = Pool::put(&pool, self.key.clone(), value);
            if result.is_ok() {
                if let Some(events) = &self.debug_events {
                    events.record(DebugEvent::ConnectionPooled { origin: self.key.origin() });
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckoutPhase {
    RacingDial,
    WaitingForCapacity,
}

pub(crate) struct Checkout<T: Poolable, K: Key> {
    key: K,
    pool: Pool<T, K>,
    waiter: Option<oneshot::Receiver<Connection<T, K>>>,
    slot_waiter: Option<SlotWaiter>,
    phase: CheckoutPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckoutError {
    PoolDisabled,
    NoLongerWanted,
    ClosedValue,
}

impl CheckoutError {
    pub(crate) fn is_cancellation(self) -> bool {
        matches!(self, Self::ClosedValue)
    }
}

impl fmt::Display for CheckoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PoolDisabled => "connection pool is disabled",
            Self::NoLongerWanted => "pool checkout was cancelled",
            Self::ClosedValue => "checked-out connection was closed",
        })
    }
}

impl std::error::Error for CheckoutError {}

impl<T: Poolable, K: Key> Checkout<T, K> {
    /// Enables capacity retries after the competing dial has failed to
    /// reserve a slot. Before that boundary, a release must not cancel the
    /// checkout's own connection establishment.
    pub(crate) fn wait_for_capacity(mut self) -> Self {
        self.phase = CheckoutPhase::WaitingForCapacity;
        self
    }

    fn poll_waiter(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Pooled<T, K>, CheckoutError>>> {
        let Some(mut receiver) = self.waiter.take() else {
            return Poll::Ready(None);
        };
        match Pin::new(&mut receiver).poll(cx) {
            Poll::Ready(Ok(value)) if value.value.is_open() => {
                Poll::Ready(Some(Ok(self.pool.reuse(&self.key, value))))
            }
            Poll::Ready(Ok(_)) => Poll::Ready(Some(Err(CheckoutError::ClosedValue))),
            Poll::Ready(Err(_)) => Poll::Ready(Some(Err(CheckoutError::NoLongerWanted))),
            Poll::Pending => {
                if let Some(waiter) = &self.slot_waiter {
                    waiter.waker.register(cx.waker());
                }
                self.waiter = Some(receiver);
                Poll::Pending
            }
        }
    }

    // A dial can lose the cap before this checkout subscribes to any slot
    // owner. Inspect current capacity as well as releases, so a later poll
    // cannot miss capacity that became available before it subscribed.
    fn poll_slot_capacity(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let Some(inner) = &self.pool.inner else {
            return Poll::Pending;
        };
        let inner = inner.lock().expect("pool mutex poisoned");
        if inner.max_h1_connections_per_host == 0 || inner.connecting.contains(&self.key) {
            return Poll::Pending;
        }
        let mut slots = inner.h1_connections.lock().expect("H1 slots mutex poisoned");
        let Some(state) = slots.get_mut(&self.key) else {
            return Poll::Ready(());
        };
        if state.count < inner.max_h1_connections_per_host {
            return Poll::Ready(());
        }
        if let Some(waiter) = &self.slot_waiter {
            if Arc::ptr_eq(&waiter.notification, &state.notification) {
                waiter.waker.register(cx.waker());
                return if waiter.notification.released.load(Ordering::Acquire) != waiter.released {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                };
            }
        }
        let waker = Arc::new(AtomicWaker::new());
        waker.register(cx.waker());
        let released = state.notification.released.load(Ordering::Acquire);
        state.waiters.retain(|waiter| waiter.strong_count() > 0);
        state.waiters.push(Arc::downgrade(&waker));
        self.slot_waiter = Some(SlotWaiter {
            notification: state.notification.clone(),
            released,
            waker,
        });
        Poll::Pending
    }

    fn checkout(&mut self, cx: &mut Context<'_>) -> Option<Pooled<T, K>> {
        let mut discarded = Vec::new();
        let entry = {
            let inner = self.pool.inner.as_ref()?;
            let mut inner = inner.lock().expect("pool mutex poisoned");
            let now = inner.now();
            let expiration = Expiration(inner.timeout);
            let debug_events = inner.debug_events.clone();
            let maybe_entry = inner.idle.get_mut(&self.key).and_then(|entries| {
                let entry = IdlePopper {
                    _key: &self.key,
                    list: entries,
                }
                .pop(expiration, now, debug_events.as_ref(), &mut discarded);
                entry.map(|entry| (entry, entries.is_empty()))
            });
            let (entry, remove) =
                maybe_entry.map_or((None, true), |(entry, empty)| (Some(entry), empty));
            if remove {
                inner.idle.remove(&self.key);
            }
            if entry.is_none() && self.waiter.is_none() {
                let (sender, mut receiver) = oneshot::channel();
                inner
                    .waiters
                    .entry(self.key.clone())
                    .or_default()
                    .push_back(sender);
                assert!(Pin::new(&mut receiver).poll(cx).is_pending());
                self.waiter = Some(receiver);
            }
            entry
        };
        if entry.is_none() && self.waiter.is_some() && self.slot_waiter.is_none() {
            let _ = self.poll_slot_capacity(cx);
        }
        // Discarding a counted socket wakes other checkouts. Release the pool
        // mutex before dropping those values so a wake callback can reenter.
        drop(discarded);
        entry.map(|entry| self.pool.reuse(&self.key, entry.value))
    }
}

impl<T: Poolable, K: Key> Future for Checkout<T, K> {
    type Output = Result<Pooled<T, K>, CheckoutError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(Some(result)) = self.poll_waiter(cx) {
            self.slot_waiter = None;
            return Poll::Ready(result);
        }
        if let Some(pooled) = self.checkout(cx) {
            self.slot_waiter = None;
            Poll::Ready(Ok(pooled))
        } else if !self.pool.is_enabled() {
            Poll::Ready(Err(CheckoutError::PoolDisabled))
        } else if self.phase == CheckoutPhase::WaitingForCapacity
            && self.poll_slot_capacity(cx).is_ready()
        {
            drop(self.waiter.take());
            if let Some(inner) = &self.pool.inner {
                inner.lock().expect("pool mutex poisoned").clean_waiters(&self.key);
            }
            self.slot_waiter = None;
            Poll::Ready(Err(CheckoutError::NoLongerWanted))
        } else {
            Poll::Pending
        }
    }
}

impl<T: Poolable, K: Key> Drop for Checkout<T, K> {
    fn drop(&mut self) {
        if self.waiter.take().is_some() {
            if let Some(inner) = &self.pool.inner {
                if let Ok(mut inner) = inner.lock() {
                    inner.clean_waiters(&self.key);
                }
            }
        }
    }
}

/// Drop guard for an in-progress connection establishment.
pub(crate) struct Connecting<T: Poolable, K: Key> {
    key: K,
    pool: WeakOpt<Mutex<Inner<T, K>>>,
    h1_slot: Option<H1Slot<K>>,
}

impl<T: Poolable, K: Key> Connecting<T, K> {
    /// An auto protocol connection that negotiated H2 releases its temporary
    /// H1 slot and converts to the shared H2 marker atomically. If another
    /// connection won, `None` asks the caller to wait for the winner rather
    /// than stampeding another H2 session.
    pub(crate) fn alpn_h2(mut self, pool: &Pool<T, K>) -> Option<Self> {
        drop(self.h1_slot.take());
        self.pool = WeakOpt::none();
        pool.connecting(&self.key, Protocol::Http2)
    }
}

impl<T: Poolable, K: Key> Drop for Connecting<T, K> {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            let waiters = pool.lock().ok().and_then(|mut inner| inner.connecting_cancelled(&self.key, self.h1_slot.is_some()));
            drop(waiters);
        }
    }
}

#[derive(Clone, Copy)]
struct Expiration(Option<Duration>);

impl Expiration {
    fn expires(self, idle_at: Instant, now: Instant) -> bool {
        self.0
            .map(|timeout| now.saturating_duration_since(idle_at) > timeout)
            .unwrap_or(false)
    }
}

struct IdleTask<T: Poolable, K: Key> {
    timer: Arc<dyn hyper::rt::Timer + Send + Sync>,
    duration: Duration,
    pool: WeakOpt<Mutex<Inner<T, K>>>,
    pool_drop_notifier: oneshot::Receiver<Infallible>,
}

impl<T: Poolable, K: Key> IdleTask<T, K> {
    async fn run(self) {
        let mut sleep = self.timer.sleep_until(self.timer.now() + self.duration);
        let mut pool_drop = self.pool_drop_notifier;
        loop {
            match future::select(&mut pool_drop, &mut sleep).await {
                future::Either::Left(_) => break,
                future::Either::Right(((), _)) => {
                    if let Some(pool) = self.pool.upgrade() {
                        let discarded = pool.lock().expect("pool mutex poisoned").clear_expired();
                        drop(discarded);
                    }
                    self.timer
                        .reset(&mut sleep, self.timer.now() + self.duration);
                }
            }
        }
    }
}

struct WeakOpt<T>(Option<Weak<T>>);

impl<T> WeakOpt<T> {
    fn none() -> Self {
        Self(None)
    }

    fn downgrade(value: &Arc<T>) -> Self {
        Self(Some(Arc::downgrade(value)))
    }

    fn upgrade(&self) -> Option<Arc<T>> {
        self.0.as_ref().and_then(Weak::upgrade)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use futures_lite::future::block_on;

    use super::{
        Checkout, CheckoutError, Config, Connecting, Pool, Poolable, Protocol, Reservation, WeakOpt,
    };
    use h12tiny_core::runtime::{AsyncIoTimer, BoxExecutor, BoxSendFuture};

    #[derive(Debug, PartialEq, Eq)]
    struct Unique(u8);

    impl Poolable for Unique {
        fn is_open(&self) -> bool {
            true
        }

        fn reserve(self) -> Reservation<Self> {
            Reservation::Unique(self)
        }

        fn can_share(&self) -> bool {
            false
        }
    }

    #[cfg(feature = "http2")]
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Shared(u8);

    #[cfg(feature = "http2")]
    impl Poolable for Shared {
        fn is_open(&self) -> bool {
            true
        }

        fn reserve(self) -> Reservation<Self> {
            Reservation::Shared(self.clone(), self)
        }

        fn can_share(&self) -> bool {
            true
        }
    }

    #[derive(Clone)]
    struct Noop;

    impl<F> hyper::rt::Executor<F> for Noop
    where
        F: Future<Output = ()> + Send + 'static,
    {
        fn execute(&self, _: F) {}
    }

    #[derive(Clone)]
    struct SmolExecutor;

    impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
        fn execute(&self, future: BoxSendFuture) {
            smol::spawn(future).detach();
        }
    }

    fn pool() -> Pool<Unique, String> {
        Pool::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host: usize::MAX,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        )
    }

    fn capped_h1_pool(max_h1_connections_per_host: usize) -> Pool<Unique, String> {
        Pool::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        )
    }

    fn connecting(key: String) -> Connecting<Unique, String> {
        Connecting {
            key,
            pool: WeakOpt::none(),
            h1_slot: None,
        }
    }

    fn poll_once<T: Poolable>(
        checkout: &mut Checkout<T, String>,
    ) -> Poll<Result<super::Pooled<T, String>, CheckoutError>> {
        let mut cx = Context::from_waker(Waker::noop());
        Pin::new(checkout).poll(&mut cx)
    }

    #[test]
    fn unique_reservation_returns_to_idle_pool_on_drop() {
        let pool = pool();
        let key = "example.test".to_owned();
        drop(pool.pooled(connecting(key.clone()), Unique(7)));
        let checked_out = block_on(pool.checkout(key)).unwrap();
        assert_eq!(*checked_out, Unique(7));
    }

    #[test]
    fn h2_connecting_marker_is_released_when_owner_is_cancelled() {
        let pool = pool();
        let key = "example.test".to_owned();
        let owner = pool.connecting(&key, Protocol::Http2).unwrap();
        assert!(pool.connecting(&key, Protocol::Http2).is_none());
        drop(owner);
        assert!(pool.connecting(&key, Protocol::Http2).is_some());
    }

    #[test]
    fn dropped_checkout_removes_its_waiter() {
        let pool = pool();
        let key = "example.test".to_owned();
        let mut checkout = pool.checkout(key.clone());
        assert!(poll_once(&mut checkout).is_pending());
        assert_eq!(
            pool.inner.as_ref().unwrap().lock().unwrap().waiters[&key].len(),
            1
        );
        drop(checkout);
        assert!(!pool
            .inner
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .waiters
            .contains_key(&key));
    }

    #[test]
    fn cancelled_h2_establishment_wakes_waiter_and_allows_retry() {
        let pool = pool();
        let key = "example.test".to_owned();
        let owner = pool.connecting(&key, Protocol::Http2).unwrap();
        let mut checkout = pool.checkout(key.clone());
        assert!(poll_once(&mut checkout).is_pending());

        // Dropping the sole establishment owner clears the marker and drops
        // parked senders. A later client attempt owns a fresh marker instead
        // of inheriting a stranded waiter.
        drop(owner);
        assert!(matches!(
            poll_once(&mut checkout),
            Poll::Ready(Err(CheckoutError::NoLongerWanted))
        ));
        assert!(pool.connecting(&key, Protocol::Http2).is_some());
    }

    #[test]
    fn max_idle_per_host_caps_unique_h1_connections() {
        let pool = pool();
        let key = "example.test".to_owned();
        for value in [1, 2, 3] {
            drop(pool.pooled(connecting(key.clone()), Unique(value)));
        }
        assert_eq!(
            pool.inner.as_ref().unwrap().lock().unwrap().idle[&key].len(),
            2
        );
    }

    #[test]
    fn h1_connection_cap_includes_connecting_and_idle_connections() {
        let pool = capped_h1_pool(1);
        let key = "example.test".to_owned();
        let connecting = pool.connecting(&key, Protocol::Http1).unwrap();
        assert!(pool.connecting(&key, Protocol::Http1).is_none());

        drop(pool.pooled(connecting, Unique(7)));
        assert!(pool.connecting(&key, Protocol::Http1).is_none());

        let checked_out = block_on(pool.checkout(key.clone())).unwrap();
        assert_eq!(*checked_out, Unique(7));
        drop(checked_out);
        assert!(pool.connecting(&key, Protocol::Http1).is_none());
    }

    #[test]
    fn established_h1_connection_is_returned_to_a_waiter_at_the_connection_cap() {
        let pool = capped_h1_pool(1);
        let key = "example.test".to_owned();
        let connecting = pool.connecting(&key, Protocol::Http1).unwrap();
        let mut checkout = pool.checkout(key);
        assert!(poll_once(&mut checkout).is_pending());

        drop(pool.pooled(connecting, Unique(7)));
        let checked_out = match poll_once(&mut checkout) {
            Poll::Ready(Ok(checked_out)) => checked_out,
            _ => panic!("established H1 connection did not wake its waiter"),
        };
        assert_eq!(*checked_out, Unique(7));
    }

    #[test]
    fn cancelled_h1_establishment_wakes_waiters_and_releases_its_slot() {
        let pool = capped_h1_pool(1);
        let key = "example.test".to_owned();
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        let mut checkout = pool.checkout(key.clone());
        assert!(poll_once(&mut checkout).is_pending());

        drop(owner);
        assert!(matches!(
            poll_once(&mut checkout),
            Poll::Ready(Err(CheckoutError::NoLongerWanted))
        ));
        assert!(pool.connecting(&key, Protocol::Http1).is_some());
    }

    #[derive(Debug)]
    struct Closed;

    impl Poolable for Closed {
        fn is_open(&self) -> bool {
            false
        }

        fn reserve(self) -> Reservation<Self> {
            Reservation::Unique(self)
        }

        fn can_share(&self) -> bool {
            false
        }
    }

    #[test]
    fn closed_values_are_not_reinserted() {
        let pool = Pool::<Closed, String>::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host: 1,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        );
        let key = "example.test".to_owned();
        let connecting = pool.connecting(&key, Protocol::Http1).unwrap();
        drop(pool.pooled(connecting, Closed));
        assert!(!pool
            .inner
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .idle
            .contains_key(&key));
        assert!(pool.connecting(&key, Protocol::Http1).is_some());
    }

    #[test]
    fn checkout_evicts_expired_idle_connections() {
        let pool = Pool::<Unique, String>::new(
            Config {
                idle_timeout: Some(Duration::from_millis(1)),
                max_idle_per_host: 2,
                max_h1_connections_per_host: usize::MAX,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        );
        let key = "example.test".to_owned();
        drop(pool.pooled(connecting(key.clone()), Unique(7)));
        std::thread::sleep(Duration::from_millis(5));
        let mut checkout = pool.checkout(key.clone());
        assert!(poll_once(&mut checkout).is_pending());
        assert!(!pool
            .inner
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .idle
            .contains_key(&key));
    }

    #[test]
    fn idle_timer_evicts_connections_without_a_later_checkout() {
        smol::block_on(async {
            let pool = Pool::<Unique, String>::new(
                Config {
                    idle_timeout: Some(Duration::from_millis(1)),
                    max_idle_per_host: 2,
                    max_h1_connections_per_host: usize::MAX,
                },
                BoxExecutor::new(SmolExecutor),
                Some(std::sync::Arc::new(AsyncIoTimer)),
                None,
            );
            let key = "example.test".to_owned();
            drop(pool.pooled(connecting(key.clone()), Unique(7)));
            assert!(pool
                .inner
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .idle
                .contains_key(&key));

            // The recurring task intentionally uses a conservative 90 ms
            // minimum interval to avoid a busy timer for tiny user-supplied
            // values. No checkout is performed after this wait.
            async_io::Timer::after(Duration::from_millis(120)).await;
            assert!(!pool
                .inner
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .idle
                .contains_key(&key));
        });
    }

    #[cfg(feature = "http2")]
    #[test]
    fn auto_connection_releases_its_h1_slot_when_alpn_selects_h2() {
        let pool = Pool::<Shared, String>::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host: 1,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        );
        let key = "example.test".to_owned();
        let h2_connecting = pool
            .connecting(&key, Protocol::Auto)
            .unwrap()
            .alpn_h2(&pool)
            .unwrap();

        assert!(pool.connecting(&key, Protocol::Http1).is_some());
        drop(h2_connecting);
    }

    #[cfg(feature = "http2")]
    #[test]
    fn shared_reservation_remains_available_while_an_h2_stream_is_checked_out() {
        let pool = Pool::<Shared, String>::new(
            Config {
                idle_timeout: None,
                max_idle_per_host: 2,
                max_h1_connections_per_host: usize::MAX,
            },
            BoxExecutor::new(Noop),
            None,
            None,
        );
        let key = "example.test".to_owned();
        let owner = Connecting {
            key: key.clone(),
            pool: WeakOpt::none(),
            h1_slot: None,
        };
        let first = pool.pooled(owner, Shared(9));
        let second = block_on(pool.checkout(key)).unwrap();
        assert_eq!(*first, Shared(9));
        assert_eq!(*second, Shared(9));
    }
}

#[cfg(test)]
mod slot_regressions {
    use super::{Checkout, CheckoutError, Config, Pool, Poolable, Pooled, Protocol, Reservation};
    use h12tiny_core::runtime::BoxExecutor;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake};
    use std::time::Duration;

    #[derive(Clone)]
    struct Unique(Arc<AtomicBool>);
    impl Poolable for Unique {
        fn is_open(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }

        fn reserve(self) -> Reservation<Self> {
            Reservation::Unique(self)
        }

        fn can_share(&self) -> bool {
            false
        }
    }
    struct Noop;
    impl<F: Future<Output = ()> + Send + 'static> hyper::rt::Executor<F> for Noop {
        fn execute(&self, _: F) {}
    }
    #[derive(Default)]
    struct WakeCounter(AtomicUsize);
    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn poll(checkout: &mut Checkout<Unique, String>) -> Poll<Result<Pooled<Unique, String>, CheckoutError>> {
        let waker = Arc::new(WakeCounter::default()).into();
        Pin::new(checkout).poll(&mut Context::from_waker(&waker))
    }
    fn pool() -> Pool<Unique, String> {
        Pool::new(Config {
            idle_timeout: Some(Duration::from_secs(1)),
            max_idle_per_host: 1,
            max_h1_connections_per_host: 1,
        }, BoxExecutor::new(Noop), None, None)
    }
    fn socket() -> Unique {
        Unique(Arc::new(AtomicBool::new(true)))
    }

    #[test]
    fn checkout_eviction_releases_h1_slot() {
        let pool = pool();
        let key = "origin".to_owned();
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        drop(pool.pooled(owner, socket()));
        pool.inner.as_ref().unwrap().lock().unwrap().idle.get_mut(&key).unwrap()[0]
            .idle_at -= Duration::from_secs(2);
        let mut checkout = pool.checkout(key.clone());
        assert!(poll(&mut checkout).is_pending());
        assert!(pool.connecting(&key, Protocol::Http1).is_some(), "expired idle socket leaked its H1 slot");
    }
    #[test]
    fn timer_eviction_releases_h1_slot() {
        let pool = pool();
        let key = "origin".to_owned();
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        drop(pool.pooled(owner, socket()));
        let discarded = {
            let mut inner = pool.inner.as_ref().unwrap().lock().unwrap();
            inner.idle.get_mut(&key).unwrap()[0].idle_at -= Duration::from_secs(2);
            inner.clear_expired()
        };
        drop(discarded);
        assert!(pool.connecting(&key, Protocol::Http1).is_some(), "timer-evicted socket leaked its H1 slot");
    }
    #[test]
    fn canceled_handoff_releases_h1_slot() {
        for waiting_for_capacity in [false, true] {
            let pool = pool();
            let key = "origin".to_owned();
            let owner = pool.connecting(&key, Protocol::Http1).unwrap();
            let held = pool.pooled(owner, socket());
            let mut checkout = pool.checkout(key.clone());
            assert!(poll(&mut checkout).is_pending());
            if waiting_for_capacity {
                checkout = checkout.wait_for_capacity();
            }
            drop(held);
            drop(checkout);
            assert!(pool.connecting(&key, Protocol::Http1).is_some(), "canceling a delivered waiter leaked its H1 slot");
        }
    }
    #[test]
    fn closed_socket_release_wakes_waiter() {
        let pool = pool();
        let key = "origin".to_owned();
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        let socket = socket();
        let open = socket.0.clone();
        let held = pool.pooled(owner, socket);
        let mut checkout = pool.checkout(key.clone());
        assert!(poll(&mut checkout).is_pending());
        checkout = checkout.wait_for_capacity();
        open.store(false, Ordering::SeqCst);
        drop(held);
        assert!(matches!(poll(&mut checkout), Poll::Ready(Err(CheckoutError::NoLongerWanted))), "slot release left the capped-origin waiter parked");
    }
    #[test]
    fn closed_handoff_releases_the_slot_exactly_once() {
        for waiting_for_capacity in [false, true] {
            let pool = pool();
            let key = "origin".to_owned();
            let owner = pool.connecting(&key, Protocol::Http1).unwrap();
            let socket = socket();
            let open = socket.0.clone();
            let held = pool.pooled(owner, socket);
            let mut checkout = pool.checkout(key.clone());
            assert!(poll(&mut checkout).is_pending());
            if waiting_for_capacity {
                checkout = checkout.wait_for_capacity();
            }
            drop(held);
            open.store(false, Ordering::SeqCst);
            assert!(matches!(poll(&mut checkout), Poll::Ready(Err(CheckoutError::ClosedValue))));
            drop(checkout);
            let replacement = pool.connecting(&key, Protocol::Http1).unwrap();
            assert!(pool.connecting(&key, Protocol::Http1).is_none());
            drop(replacement);
            assert!(pool.inner.as_ref().unwrap().lock().unwrap()
                .h1_connections.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn cancelled_waiter_before_handoff_keeps_the_live_socket_counted() {
        for waiting_for_capacity in [false, true] {
            let pool = pool();
            let key = "origin".to_owned();
            let owner = pool.connecting(&key, Protocol::Http1).unwrap();
            let held = pool.pooled(owner, socket());
            let mut checkout = pool.checkout(key.clone());
            assert!(poll(&mut checkout).is_pending());
            if waiting_for_capacity {
                checkout = checkout.wait_for_capacity();
            }
            drop(checkout);
            drop(held);
            assert!(pool.connecting(&key, Protocol::Http1).is_none());
            assert!(matches!(poll(&mut pool.checkout(key)), Poll::Ready(Ok(_))));
        }
    }

    #[test]
    fn released_slot_wakes_all_parked_checkouts_once() {
        let pool = pool();
        let key = "origin".to_owned();
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        let socket = socket();
        let open = socket.0.clone();
        let held = pool.pooled(owner, socket);
        let mut first = pool.checkout(key.clone());
        let mut second = pool.checkout(key.clone());
        let counter = Arc::new(WakeCounter::default());
        let waker = counter.clone().into();
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut first).poll(&mut cx).is_pending());
        assert!(Pin::new(&mut second).poll(&mut cx).is_pending());
        first = first.wait_for_capacity();
        second = second.wait_for_capacity();
        open.store(false, Ordering::SeqCst);
        drop(held);
        assert_eq!(counter.0.load(Ordering::SeqCst), 2);
        assert!(matches!(poll(&mut first), Poll::Ready(Err(CheckoutError::NoLongerWanted))));
        assert!(matches!(poll(&mut second), Poll::Ready(Err(CheckoutError::NoLongerWanted))));
        let replacement = pool.connecting(&key, Protocol::Http1).unwrap();
        assert!(pool.connecting(&key, Protocol::Http1).is_none());
        let mut later = pool.checkout(key);
        assert!(poll(&mut later).is_pending());
        drop(replacement);
    }

    struct LockProbe {
        pool: Pool<Unique, String>,
        wakes: AtomicUsize,
    }

    struct HandoffProbe {
        pool: Pool<Unique, String>,
        locked: AtomicBool,
        wakes: AtomicUsize,
    }

    impl Wake for HandoffProbe {
        fn wake(self: Arc<Self>) {
            self.locked.fetch_or(self.pool.inner.as_ref().unwrap().try_lock().is_err(), Ordering::SeqCst);
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn handoff_wakes_after_unlocking_the_pool() {
        let pool = pool();
        let key = "origin".to_owned();
        let held = pool.pooled(pool.connecting(&key, Protocol::Http1).unwrap(), socket());
        let mut checkout = pool.checkout(key);
        let probe = Arc::new(HandoffProbe { pool: pool.clone(), locked: AtomicBool::new(false), wakes: AtomicUsize::new(0) });
        let waker = probe.clone().into();
        assert!(Pin::new(&mut checkout).poll(&mut Context::from_waker(&waker)).is_pending());
        drop(held);
        assert_eq!(probe.wakes.load(Ordering::SeqCst), 1);
        assert!(!probe.locked.load(Ordering::SeqCst));
    }

    #[test]
    fn connecting_cancellation_wakes_after_unlocking_the_pool() {
        let pool = pool();
        let key = "origin".to_owned();
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        let mut checkout = pool.checkout(key);
        let probe = Arc::new(HandoffProbe { pool: pool.clone(), locked: AtomicBool::new(false), wakes: AtomicUsize::new(0) });
        let waker = probe.clone().into();
        assert!(Pin::new(&mut checkout).poll(&mut Context::from_waker(&waker)).is_pending());
        drop(owner);
        assert!(probe.wakes.load(Ordering::SeqCst) > 0);
        assert!(!probe.locked.load(Ordering::SeqCst));
    }

    impl Wake for LockProbe {
        fn wake(self: Arc<Self>) {
            assert!(self.pool.inner.as_ref().unwrap().try_lock().is_ok(),
                "slot release woke a checkout while the pool mutex was held");
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn checkout_eviction_wakes_after_unlocking_the_pool() {
        let pool = pool();
        let key = "origin".to_owned();
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        drop(pool.pooled(owner, socket()));
        pool.inner.as_ref().unwrap().lock().unwrap().idle.get_mut(&key).unwrap()[0]
            .idle_at -= Duration::from_secs(2);
        let probe = Arc::new(LockProbe { pool: pool.clone(), wakes: AtomicUsize::new(0) });
        let waker = probe.clone().into();
        let mut checkout = pool.checkout(key.clone());
        assert!(Pin::new(&mut checkout).poll(&mut Context::from_waker(&waker)).is_pending());
        assert_eq!(probe.wakes.load(Ordering::SeqCst), 1);
        let replacement = pool.connecting(&key, Protocol::Http1).unwrap();
        drop(checkout);
        drop(replacement);
    }

    #[test]
    fn closed_idle_connection_releases_its_h1_slot() {
        let pool = pool();
        let key = "origin".to_owned();
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        let socket = socket();
        let open = socket.0.clone();
        drop(pool.pooled(owner, socket));
        open.store(false, Ordering::SeqCst);
        let mut checkout = pool.checkout(key.clone());
        assert!(poll(&mut checkout).is_pending());
        assert!(pool.connecting(&key, Protocol::Http1).is_some());
    }

    #[test]
    fn idle_capacity_rejection_releases_only_the_discarded_slot() {
        let pool = pool();
        let key = "origin".to_owned();
        pool.inner.as_ref().unwrap().lock().unwrap().max_h1_connections_per_host = 2;
        let first = pool.pooled(pool.connecting(&key, Protocol::Http1).unwrap(), socket());
        let second = pool.pooled(pool.connecting(&key, Protocol::Http1).unwrap(), socket());
        drop(first);
        drop(second);
        let replacement = pool.connecting(&key, Protocol::Http1).unwrap();
        assert!(pool.connecting(&key, Protocol::Http1).is_none());
        assert!(matches!(poll(&mut pool.checkout(key)), Poll::Ready(Ok(_))));
        drop(replacement);
    }

    #[test]
    fn checkout_created_before_reservation_subscribes_on_repoll() {
        let pool = pool();
        let key = "origin".to_owned();
        let mut checkout = pool.checkout(key.clone());
        let counter = Arc::new(WakeCounter::default());
        let waker = counter.clone().into();
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut checkout).poll(&mut cx).is_pending());
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        let socket = socket();
        let open = socket.0.clone();
        let held = pool.pooled(owner, socket);
        assert!(pool.connecting(&key, Protocol::Http1).is_none());
        checkout = checkout.wait_for_capacity();
        assert!(Pin::new(&mut checkout).poll(&mut cx).is_pending());
        open.store(false, Ordering::SeqCst);
        drop(held);
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert!(matches!(poll(&mut checkout), Poll::Ready(Err(CheckoutError::NoLongerWanted))));
    }

    #[test]
    fn checkout_created_before_reservation_retries_if_release_precedes_repoll() {
        let pool = pool();
        let key = "origin".to_owned();
        let mut checkout = pool.checkout(key.clone());
        let counter = Arc::new(WakeCounter::default());
        let waker = counter.clone().into();
        let mut cx = Context::from_waker(&waker);
        assert!(Pin::new(&mut checkout).poll(&mut cx).is_pending());
        let owner = pool.connecting(&key, Protocol::Http1).unwrap();
        let socket = socket();
        let open = socket.0.clone();
        let held = pool.pooled(owner, socket);
        assert!(pool.connecting(&key, Protocol::Http1).is_none());
        checkout = checkout.wait_for_capacity();
        open.store(false, Ordering::SeqCst);
        drop(held);
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);
        assert!(matches!(Pin::new(&mut checkout).poll(&mut cx),
            Poll::Ready(Err(CheckoutError::NoLongerWanted))));
        let mut fresh = pool.checkout(key.clone());
        assert!(Pin::new(&mut fresh).poll(&mut cx).is_pending());
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);
        let replacement = pool.connecting(&key, Protocol::Http1).unwrap();
        drop(fresh);
        drop(replacement);
    }

    #[test]
    fn existing_checkout_stays_parked_behind_an_h2_establishment() {
        let pool = pool();
        let key = "origin".to_owned();
        let mut checkout = pool.checkout(key.clone());
        assert!(poll(&mut checkout).is_pending());
        let owner = pool.connecting(&key, Protocol::Http2).unwrap();
        checkout = checkout.wait_for_capacity();
        assert!(poll(&mut checkout).is_pending());
        assert!(poll(&mut checkout).is_pending());
        drop(owner);
        assert!(matches!(poll(&mut checkout), Poll::Ready(Err(CheckoutError::NoLongerWanted))));
    }

    #[test]
    fn late_capacity_waiter_retries_after_one_of_two_owners_releases() {
        let pool = pool();
        let key = "origin".to_owned();
        pool.inner.as_ref().unwrap().lock().unwrap().max_h1_connections_per_host = 2;
        let mut checkout = pool.checkout(key.clone());
        assert!(poll(&mut checkout).is_pending());
        let first_socket = socket();
        let first_open = first_socket.0.clone();
        let first = pool.pooled(pool.connecting(&key, Protocol::Http1).unwrap(), first_socket);
        let second = pool.pooled(pool.connecting(&key, Protocol::Http1).unwrap(), socket());
        assert!(pool.connecting(&key, Protocol::Http1).is_none());
        checkout = checkout.wait_for_capacity();
        first_open.store(false, Ordering::SeqCst);
        drop(first);
        assert!(matches!(poll(&mut checkout), Poll::Ready(Err(CheckoutError::NoLongerWanted))));
        let replacement = pool.connecting(&key, Protocol::Http1).unwrap();
        assert!(pool.connecting(&key, Protocol::Http1).is_none());
        drop(replacement);
        drop(second);
    }

    #[test]
    fn racing_checkout_keeps_its_own_dial_when_another_slot_releases() {
        for cap in [2, usize::MAX] {
            let pool = pool();
            let key = "origin".to_owned();
            pool.inner.as_ref().unwrap().lock().unwrap().max_h1_connections_per_host = cap;
            let mut checkout = pool.checkout(key.clone());
            let counter = Arc::new(WakeCounter::default());
            let waker = counter.clone().into();
            let mut cx = Context::from_waker(&waker);
            assert!(Pin::new(&mut checkout).poll(&mut cx).is_pending());
            let own_dial = pool.connecting(&key, Protocol::Http1).unwrap();
            let other_socket = socket();
            let other_open = other_socket.0.clone();
            let other = pool.pooled(pool.connecting(&key, Protocol::Http1).unwrap(), other_socket);
            assert!(Pin::new(&mut checkout).poll(&mut cx).is_pending());
            other_open.store(false, Ordering::SeqCst);
            drop(other);
            assert_eq!(counter.0.load(Ordering::SeqCst), usize::from(cap == 2));
            assert!(Pin::new(&mut checkout).poll(&mut cx).is_pending(),
                "capacity notification cancelled an active dial");
            drop(checkout);
            drop(own_dial);
        }
    }

    #[test]
    fn handoff_is_unique_in_both_checkout_phases() {
        for waiting_for_capacity in [false, true] {
            let pool = pool();
            let key = "origin".to_owned();
            let held = pool.pooled(pool.connecting(&key, Protocol::Http1).unwrap(), socket());
            let mut checkout = pool.checkout(key.clone());
            assert!(poll(&mut checkout).is_pending());
            if waiting_for_capacity {
                checkout = checkout.wait_for_capacity();
            }
            drop(held);
            let checked_out = match poll(&mut checkout) {
                Poll::Ready(Ok(value)) => value,
                _ => panic!("handoff did not deliver the unique connection"),
            };
            assert!(pool.connecting(&key, Protocol::Http1).is_none());
            drop(checked_out);
            assert!(pool.connecting(&key, Protocol::Http1).is_none());
        }
    }

}
