//! Route selection and load balancing.
//!
//! [`Router`] holds a set of endpoints, tracks each one's health, and picks
//! which should serve the next request. Failing endpoints are withdrawn
//! automatically and probed for recovery, so a dead backend stops receiving
//! traffic without anyone reconfiguring anything.

use std::time::Instant;

use crate::health::{Health, HealthPolicy, HealthTracker};

/// How the router chooses among healthy endpoints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Strategy {
    /// Visit endpoints in order, one after another.
    ///
    /// Fair when endpoints are equivalent and requests cost about the same.
    ///
    /// The default.
    #[default]
    RoundRobin,
    /// Visit endpoints in proportion to their weight.
    ///
    /// Use when backends differ in capacity.
    WeightedRoundRobin,
    /// Choose the endpoint with the fewest requests in flight.
    ///
    /// Adapts to requests of uneven cost, which round robin cannot: a backend
    /// stuck on a slow request stops attracting more work.
    LeastConnections,
}

impl std::fmt::Display for Strategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::RoundRobin => "round-robin",
            Self::WeightedRoundRobin => "weighted-round-robin",
            Self::LeastConnections => "least-connections",
        };
        f.write_str(name)
    }
}

/// Identifies an endpoint within one router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndpointId(usize);

impl EndpointId {
    /// Returns the identifier as an index.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl std::fmt::Display for EndpointId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "endpoint#{}", self.0)
    }
}

/// One routable destination.
#[derive(Debug, Clone)]
pub struct Endpoint<T> {
    id: EndpointId,
    address: T,
    weight: u32,
    in_flight: u32,
    health: HealthTracker,
}

impl<T> Endpoint<T> {
    /// Returns this endpoint's identifier.
    #[must_use]
    pub const fn id(&self) -> EndpointId {
        self.id
    }

    /// Returns the address or descriptor this endpoint routes to.
    #[must_use]
    pub const fn address(&self) -> &T {
        &self.address
    }

    /// Returns the relative capacity weight.
    #[must_use]
    pub const fn weight(&self) -> u32 {
        self.weight
    }

    /// Returns how many requests are currently in flight.
    #[must_use]
    pub const fn in_flight(&self) -> u32 {
        self.in_flight
    }

    /// Returns the health state as of `now`.
    #[must_use]
    pub fn health(&self, now: Instant) -> Health {
        self.health.health(now)
    }

    /// Returns the fraction of requests to this endpoint that succeeded.
    #[must_use]
    pub fn success_ratio(&self) -> f64 {
        self.health.success_ratio()
    }
}

/// A snapshot of router activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct RouterStats {
    /// Endpoints registered.
    pub endpoints: usize,
    /// Endpoints currently able to serve traffic.
    pub healthy: usize,
    /// Endpoints currently withdrawn.
    pub withdrawn: usize,
    /// Selections served.
    pub selections: u64,
    /// Selections that found no usable endpoint.
    pub exhausted: u64,
    /// Recovery probes issued.
    pub probes: u64,
}

impl RouterStats {
    /// Returns `true` if no endpoint can currently serve traffic.
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        self.healthy == 0
    }
}

/// Selects endpoints, avoiding unhealthy ones.
///
/// # Examples
///
/// ```
/// use std::time::Instant;
/// use nexusnet_router::{Router, Strategy};
///
/// let now = Instant::now();
/// let mut router: Router<&str> = Router::new(Strategy::RoundRobin);
///
/// let primary = router.add("10.0.0.1:9000");
/// let secondary = router.add("10.0.0.2:9000");
///
/// // Traffic alternates between them.
/// assert_eq!(router.select(now), Some(primary));
/// assert_eq!(router.select(now), Some(secondary));
///
/// // Once one fails repeatedly it is withdrawn and skipped.
/// for _ in 0..3 {
///     router.record_failure(primary, now);
/// }
/// assert_eq!(router.select(now), Some(secondary));
/// assert_eq!(router.select(now), Some(secondary));
/// ```
#[derive(Debug)]
pub struct Router<T> {
    endpoints: Vec<Endpoint<T>>,
    strategy: Strategy,
    policy: HealthPolicy,
    cursor: usize,
    /// Remaining weight credit for the endpoint at `cursor`, used by
    /// [`Strategy::WeightedRoundRobin`].
    credit: u32,
    selections: u64,
    exhausted: u64,
    probes: u64,
    next_id: usize,
}

impl<T> Router<T> {
    /// Creates an empty router using `strategy`.
    #[must_use]
    pub fn new(strategy: Strategy) -> Self {
        Self::with_policy(strategy, HealthPolicy::new())
    }

    /// Creates an empty router with an explicit health policy.
    #[must_use]
    pub fn with_policy(strategy: Strategy, policy: HealthPolicy) -> Self {
        Self {
            endpoints: Vec::new(),
            strategy,
            policy,
            cursor: 0,
            credit: 0,
            selections: 0,
            exhausted: 0,
            probes: 0,
            next_id: 0,
        }
    }

    /// Adds an endpoint with the default weight of 1.
    pub fn add(&mut self, address: T) -> EndpointId {
        self.add_weighted(address, 1)
    }

    /// Adds an endpoint with an explicit capacity weight.
    ///
    /// A zero weight is raised to one: an endpoint that can never be chosen
    /// should be removed rather than registered.
    pub fn add_weighted(&mut self, address: T, weight: u32) -> EndpointId {
        let id = EndpointId(self.next_id);
        self.next_id += 1;

        self.endpoints.push(Endpoint {
            id,
            address,
            weight: weight.max(1),
            in_flight: 0,
            health: HealthTracker::new(self.policy),
        });

        id
    }

    /// Removes an endpoint, returning its address if it was present.
    pub fn remove(&mut self, id: EndpointId) -> Option<T> {
        let index = self.endpoints.iter().position(|e| e.id == id)?;
        let removed = self.endpoints.remove(index);

        // The cursor indexes into a vector that just shrank.
        if self.cursor >= self.endpoints.len() {
            self.cursor = 0;
        }
        self.credit = 0;

        Some(removed.address)
    }

    /// Returns the number of registered endpoints.
    #[must_use]
    pub fn len(&self) -> usize {
        self.endpoints.len()
    }

    /// Returns `true` if no endpoint is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    /// Returns the selection strategy.
    #[must_use]
    pub const fn strategy(&self) -> Strategy {
        self.strategy
    }

    /// Changes the selection strategy.
    pub fn set_strategy(&mut self, strategy: Strategy) {
        self.strategy = strategy;
        self.credit = 0;
    }

    /// Returns an endpoint by identifier.
    #[must_use]
    pub fn endpoint(&self, id: EndpointId) -> Option<&Endpoint<T>> {
        self.endpoints.iter().find(|e| e.id == id)
    }

    /// Returns the address an identifier routes to.
    #[must_use]
    pub fn address(&self, id: EndpointId) -> Option<&T> {
        self.endpoint(id).map(Endpoint::address)
    }

    /// Returns every registered endpoint.
    #[must_use]
    pub fn endpoints(&self) -> &[Endpoint<T>] {
        &self.endpoints
    }

    /// Returns a snapshot of router activity as of `now`.
    #[must_use]
    pub fn stats(&self, now: Instant) -> RouterStats {
        let healthy = self
            .endpoints
            .iter()
            .filter(|e| e.health.is_available(now))
            .count();

        RouterStats {
            endpoints: self.endpoints.len(),
            healthy,
            withdrawn: self.endpoints.len() - healthy,
            selections: self.selections,
            exhausted: self.exhausted,
            probes: self.probes,
        }
    }

    /// Selects an endpoint for the next request.
    ///
    /// Returns `None` when every endpoint is withdrawn and none is due a probe.
    /// Callers should treat that as backpressure rather than routing anyway.
    pub fn select(&mut self, now: Instant) -> Option<EndpointId> {
        // A recovering endpoint takes priority: the probe is what restores
        // capacity, and delaying it keeps the pool smaller than it needs to be.
        if let Some(id) = self.take_probe(now) {
            self.selections += 1;
            self.probes += 1;
            return Some(id);
        }

        let selected = match self.strategy {
            Strategy::RoundRobin => self.select_round_robin(now),
            Strategy::WeightedRoundRobin => self.select_weighted(now),
            Strategy::LeastConnections => self.select_least_connections(now),
        };

        match selected {
            Some(id) => {
                self.selections += 1;
                Some(id)
            }
            None => {
                self.exhausted += 1;
                None
            }
        }
    }

    /// Returns an endpoint due a recovery probe, if any.
    fn take_probe(&mut self, now: Instant) -> Option<EndpointId> {
        let index = self
            .endpoints
            .iter()
            .position(|e| e.health.accepts_probe(now))?;

        Some(self.endpoints[index].id)
    }

    /// Advances the cursor to the next healthy endpoint.
    fn select_round_robin(&mut self, now: Instant) -> Option<EndpointId> {
        let count = self.endpoints.len();
        if count == 0 {
            return None;
        }

        for step in 0..count {
            let index = (self.cursor + step) % count;

            if self.endpoints[index].health.is_available(now) {
                self.cursor = (index + 1) % count;
                return Some(self.endpoints[index].id);
            }
        }

        None
    }

    /// Serves each endpoint for as many turns as its weight before moving on.
    fn select_weighted(&mut self, now: Instant) -> Option<EndpointId> {
        let count = self.endpoints.len();
        if count == 0 {
            return None;
        }

        for _ in 0..count {
            if self.cursor >= count {
                self.cursor = 0;
            }

            let endpoint = &self.endpoints[self.cursor];

            if !endpoint.health.is_available(now) {
                self.cursor = (self.cursor + 1) % count;
                self.credit = 0;
                continue;
            }

            if self.credit == 0 {
                self.credit = endpoint.weight;
            }

            self.credit -= 1;
            let id = endpoint.id;

            if self.credit == 0 {
                self.cursor = (self.cursor + 1) % count;
            }

            return Some(id);
        }

        None
    }

    /// Chooses the healthy endpoint with the least work outstanding.
    ///
    /// Ties break toward the higher weight, then the lower identifier, so the
    /// choice is deterministic rather than dependent on iteration order.
    fn select_least_connections(&mut self, now: Instant) -> Option<EndpointId> {
        self.endpoints
            .iter()
            .filter(|e| e.health.is_available(now))
            .min_by(|a, b| {
                // Normalize by weight so a bigger backend accepts more work
                // before it looks equally loaded.
                let a_load = f64::from(a.in_flight) / f64::from(a.weight);
                let b_load = f64::from(b.in_flight) / f64::from(b.weight);

                a_load
                    .partial_cmp(&b_load)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.weight.cmp(&a.weight))
                    .then_with(|| a.id.cmp(&b.id))
            })
            .map(|e| e.id)
    }

    /// Records that a request to `id` has begun.
    ///
    /// Required for [`Strategy::LeastConnections`] to mean anything; harmless
    /// otherwise.
    pub fn record_started(&mut self, id: EndpointId) {
        if let Some(endpoint) = self.endpoints.iter_mut().find(|e| e.id == id) {
            endpoint.in_flight = endpoint.in_flight.saturating_add(1);
        }
    }

    /// Records that a request to `id` finished, successfully or not.
    fn record_finished(&mut self, id: EndpointId) {
        if let Some(endpoint) = self.endpoints.iter_mut().find(|e| e.id == id) {
            endpoint.in_flight = endpoint.in_flight.saturating_sub(1);
        }
    }

    /// Records a successful request to `id`.
    pub fn record_success(&mut self, id: EndpointId, now: Instant) {
        self.record_finished(id);

        if let Some(endpoint) = self.endpoints.iter_mut().find(|e| e.id == id) {
            endpoint.health.record_success(now);
        }
    }

    /// Records a failed request to `id`.
    ///
    /// Enough consecutive failures withdraw the endpoint from selection.
    pub fn record_failure(&mut self, id: EndpointId, now: Instant) {
        self.record_finished(id);

        if let Some(endpoint) = self.endpoints.iter_mut().find(|e| e.id == id) {
            endpoint.health.record_failure(now);
        }
    }

    /// Restores every endpoint to full health.
    pub fn reset_health(&mut self) {
        for endpoint in &mut self.endpoints {
            endpoint.health.reset();
        }
    }
}

impl<T> Default for Router<T> {
    fn default() -> Self {
        Self::new(Strategy::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    fn start() -> Instant {
        Instant::now()
    }

    fn router() -> Router<&'static str> {
        Router::with_policy(
            Strategy::RoundRobin,
            HealthPolicy::new()
                .with_failure_threshold(3)
                .with_success_threshold(1)
                .with_cooldown(Duration::from_secs(30)),
        )
    }

    #[test]
    fn the_default_strategy_is_round_robin() {
        // Pinned: the default is derived, so a misplaced `#[default]` would
        // silently change how every router balances.
        assert_eq!(Strategy::default(), Strategy::RoundRobin);

        let router: Router<&str> = Router::default();
        assert_eq!(router.strategy(), Strategy::RoundRobin);
    }

    #[test]
    fn an_empty_router_selects_nothing() {
        let now = start();
        let mut router = router();

        assert!(router.is_empty());
        assert_eq!(router.select(now), None);
        assert_eq!(router.stats(now).exhausted, 1);
    }

    #[test]
    fn round_robin_visits_every_endpoint() {
        let now = start();
        let mut router = router();

        let a = router.add("a");
        let b = router.add("b");
        let c = router.add("c");

        assert_eq!(router.select(now), Some(a));
        assert_eq!(router.select(now), Some(b));
        assert_eq!(router.select(now), Some(c));
        assert_eq!(router.select(now), Some(a), "and then wraps around");
    }

    #[test]
    fn withdrawn_endpoints_are_skipped() {
        let now = start();
        let mut router = router();

        let a = router.add("a");
        let b = router.add("b");

        for _ in 0..3 {
            router.record_failure(a, now);
        }

        // Every selection now goes to the survivor.
        for _ in 0..5 {
            assert_eq!(router.select(now), Some(b));
        }

        let stats = router.stats(now);
        assert_eq!(stats.healthy, 1);
        assert_eq!(stats.withdrawn, 1);
    }

    #[test]
    fn a_recovered_endpoint_returns_to_the_rotation() {
        let now = start();
        let mut router = router();

        let a = router.add("a");
        let b = router.add("b");

        for _ in 0..3 {
            router.record_failure(a, now);
        }
        assert_eq!(router.select(now), Some(b));

        // After the cooldown, the withdrawn endpoint is probed first.
        let later = now + Duration::from_secs(31);
        assert_eq!(
            router.select(later),
            Some(a),
            "the probe is what restores capacity, so it takes priority"
        );

        // A successful probe restores it fully.
        router.record_success(a, later);
        assert!(router
            .endpoint(a)
            .expect("present")
            .health(later)
            .is_available());
        assert_eq!(router.stats(later).probes, 1);
    }

    #[test]
    fn a_failed_probe_keeps_the_endpoint_out() {
        let now = start();
        let mut router = router();

        let a = router.add("a");
        let b = router.add("b");

        for _ in 0..3 {
            router.record_failure(a, now);
        }

        let probe_time = now + Duration::from_secs(31);
        assert_eq!(router.select(probe_time), Some(a));
        router.record_failure(a, probe_time);

        // Back out of rotation, without another immediate probe.
        for _ in 0..3 {
            assert_eq!(router.select(probe_time + Duration::from_secs(1)), Some(b));
        }
    }

    #[test]
    fn total_failure_reports_exhaustion() {
        let now = start();
        let mut router = router();

        let a = router.add("a");
        let b = router.add("b");

        for _ in 0..3 {
            router.record_failure(a, now);
            router.record_failure(b, now);
        }

        assert_eq!(
            router.select(now),
            None,
            "routing to a known-dead endpoint is worse than reporting failure"
        );
        assert!(router.stats(now).is_exhausted());
    }

    #[test]
    fn weighted_selection_follows_capacity() {
        let now = start();
        let mut router: Router<&str> = Router::new(Strategy::WeightedRoundRobin);

        let big = router.add_weighted("big", 3);
        let small = router.add_weighted("small", 1);

        let mut big_count = 0;
        let mut small_count = 0;
        for _ in 0..400 {
            match router.select(now) {
                Some(id) if id == big => big_count += 1,
                Some(id) if id == small => small_count += 1,
                _ => {}
            }
        }

        let ratio = f64::from(big_count) / f64::from(small_count);
        assert!(
            (2.5..=3.5).contains(&ratio),
            "expected roughly 3:1, got {ratio:.2}:1 ({big_count} vs {small_count})"
        );
    }

    #[test]
    fn a_zero_weight_is_corrected() {
        let mut router: Router<&str> = Router::new(Strategy::WeightedRoundRobin);
        let id = router.add_weighted("zero", 0);

        assert_eq!(
            router.endpoint(id).expect("present").weight(),
            1,
            "an endpoint that can never be chosen should be removed, not registered"
        );
    }

    #[test]
    fn least_connections_avoids_a_busy_endpoint() {
        let now = start();
        let mut router: Router<&str> = Router::new(Strategy::LeastConnections);

        let busy = router.add("busy");
        let idle = router.add("idle");

        // The busy endpoint is stuck on several slow requests.
        for _ in 0..5 {
            router.record_started(busy);
        }

        assert_eq!(
            router.select(now),
            Some(idle),
            "work should go where there is capacity"
        );
    }

    #[test]
    fn least_connections_accounts_for_weight() {
        let now = start();
        let mut router: Router<&str> = Router::new(Strategy::LeastConnections);

        let big = router.add_weighted("big", 4);
        let small = router.add_weighted("small", 1);

        // Two in flight on a weight-4 backend is lighter load than one on a
        // weight-1 backend.
        router.record_started(big);
        router.record_started(big);
        router.record_started(small);

        assert_eq!(router.select(now), Some(big));
    }

    #[test]
    fn completed_requests_free_capacity() {
        let now = start();
        let mut router: Router<&str> = Router::new(Strategy::LeastConnections);

        let a = router.add("a");
        let _b = router.add("b");

        router.record_started(a);
        assert_eq!(router.endpoint(a).expect("present").in_flight(), 1);

        router.record_success(a, now);
        assert_eq!(router.endpoint(a).expect("present").in_flight(), 0);
    }

    #[test]
    fn endpoints_can_be_removed() {
        let now = start();
        let mut router = router();

        let a = router.add("a");
        let b = router.add("b");

        assert_eq!(router.remove(a), Some("a"));
        assert_eq!(router.len(), 1);
        assert!(router.endpoint(a).is_none());
        assert_eq!(router.remove(a), None, "removing twice is not an error");

        for _ in 0..3 {
            assert_eq!(router.select(now), Some(b));
        }
    }

    #[test]
    fn removing_the_endpoint_at_the_cursor_is_safe() {
        let now = start();
        let mut router = router();

        let a = router.add("a");
        let b = router.add("b");
        let c = router.add("c");

        // Advance the cursor to the end.
        router.select(now);
        router.select(now);
        router.select(now);

        router.remove(c);
        router.remove(b);

        // A stale cursor must not index out of bounds.
        assert_eq!(router.select(now), Some(a));
    }

    #[test]
    fn the_strategy_can_be_changed() {
        let now = start();
        let mut router: Router<&str> = Router::new(Strategy::RoundRobin);
        let a = router.add("a");
        let _b = router.add("b");

        router.set_strategy(Strategy::LeastConnections);
        assert_eq!(router.strategy(), Strategy::LeastConnections);
        assert_eq!(router.select(now), Some(a), "ties break deterministically");
    }

    #[test]
    fn health_can_be_restored_manually() {
        let now = start();
        let mut router = router();
        let a = router.add("a");

        for _ in 0..3 {
            router.record_failure(a, now);
        }
        assert!(router.stats(now).is_exhausted());

        router.reset_health();
        assert_eq!(router.select(now), Some(a));
    }

    #[test]
    fn success_ratios_are_reported_per_endpoint() {
        let now = start();
        let mut router = router();
        let a = router.add("a");

        for _ in 0..9 {
            router.record_success(a, now);
        }
        router.record_failure(a, now);

        let ratio = router.endpoint(a).expect("present").success_ratio();
        assert!((ratio - 0.9).abs() < 1e-9, "got {ratio}");
    }

    #[test]
    fn addresses_are_retrievable() {
        let mut router = router();
        let a = router.add("10.0.0.1:9000");

        assert_eq!(router.address(a), Some(&"10.0.0.1:9000"));
        assert_eq!(a.to_string(), "endpoint#0");
    }
}
