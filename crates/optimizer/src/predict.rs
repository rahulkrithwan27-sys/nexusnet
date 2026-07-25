//! Trend prediction and predictive scheduling.
//!
//! Every estimator elsewhere in this crate answers "what is the network doing
//! now". That is always slightly stale — by the time a change is smoothed into
//! an average, it has already happened. [`TrendPredictor`] answers a different
//! question: **which way is it heading**, and what will it be shortly.
//!
//! The distinction matters for scheduling. A sender that knows bandwidth is
//! collapsing can shed load before the queue builds, rather than discovering it
//! through timeouts. A sender that knows bandwidth is recovering can wait a
//! moment and send a large transfer into better conditions instead of dragging
//! it through the tail of a bad patch.
//!
//! ## Method
//!
//! Ordinary least-squares regression over a bounded window of recent samples.
//! The slope is the trend; extrapolating it gives the forecast. Deliberately
//! simple: with a handful of noisy samples, an elaborate model produces
//! confident nonsense, and the confidence figure here is honest about how well
//! the line actually fits.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The default number of samples retained for regression.
///
/// Enough to see through noise, few enough to react within a few seconds of a
/// genuine change.
pub const DEFAULT_WINDOW: usize = 16;

/// The default forecast horizon.
pub const DEFAULT_HORIZON: Duration = Duration::from_secs(5);

/// The direction a measurement is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Trend {
    /// Conditions are getting better.
    Improving,
    /// No meaningful change.
    Stable,
    /// Conditions are getting worse.
    Degrading,
}

impl Trend {
    /// Returns `true` if conditions are worsening.
    #[must_use]
    pub const fn is_degrading(self) -> bool {
        matches!(self, Self::Degrading)
    }

    /// Returns `true` if conditions are improving.
    #[must_use]
    pub const fn is_improving(self) -> bool {
        matches!(self, Self::Improving)
    }
}

impl std::fmt::Display for Trend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Improving => "improving",
            Self::Stable => "stable",
            Self::Degrading => "degrading",
        };
        f.write_str(name)
    }
}

/// A prediction of where a measurement is heading.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Forecast {
    /// The most recent observed value.
    pub current: f64,
    /// The predicted value at the end of the horizon.
    ///
    /// Never negative: a linear extrapolation can run below zero, which is
    /// meaningless for bandwidth.
    pub predicted: f64,
    /// The direction of change.
    pub trend: Trend,
    /// How well the trend line fits the samples, in `0.0..=1.0`.
    ///
    /// This is the coefficient of determination. Low values mean the samples
    /// are noise around a level rather than a line, and the prediction should
    /// be discounted accordingly.
    pub confidence: f64,
    /// How far ahead the prediction reaches.
    pub horizon: Duration,
    /// Rate of change per second.
    pub slope_per_second: f64,
}

impl Forecast {
    /// Returns the predicted change as a fraction of the current value.
    ///
    /// `-0.5` means a halving is expected. Returns `0.0` when the current value
    /// is zero, since no proportion is defined.
    #[must_use]
    pub fn relative_change(&self) -> f64 {
        if self.current.abs() < f64::EPSILON {
            0.0
        } else {
            (self.predicted - self.current) / self.current
        }
    }

    /// Returns `true` if the forecast is worth acting on.
    ///
    /// Requires both a meaningful predicted change and a trend line that
    /// actually fits. Acting on a confident-looking prediction drawn through
    /// noise is worse than not predicting at all.
    #[must_use]
    pub fn is_actionable(&self) -> bool {
        self.confidence >= 0.5 && self.relative_change().abs() >= 0.15
    }
}

/// Predicts where a measurement is heading from its recent history.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
/// use nexusnet_optimizer::{TrendPredictor, Trend};
///
/// let start = Instant::now();
/// let mut predictor = TrendPredictor::new();
///
/// // Bandwidth falling steadily by 100 bytes per second.
/// for step in 0..12_u64 {
///     let value = 2000.0 - (step as f64) * 100.0;
///     predictor.record(value, start + Duration::from_secs(step));
/// }
///
/// let forecast = predictor.forecast(start + Duration::from_secs(12)).expect("samples exist");
/// assert_eq!(forecast.trend, Trend::Degrading);
/// assert!(forecast.predicted < forecast.current);
/// ```
#[derive(Debug, Clone)]
pub struct TrendPredictor {
    /// Samples as `(seconds since the first sample, value)`.
    samples: VecDeque<(f64, f64)>,
    capacity: usize,
    horizon: Duration,
    origin: Option<Instant>,
    /// The fraction of change per second below which a trend counts as stable.
    stability_threshold: f64,
}

impl TrendPredictor {
    /// Creates a predictor with the default window and horizon.
    #[must_use]
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(DEFAULT_WINDOW),
            capacity: DEFAULT_WINDOW,
            horizon: DEFAULT_HORIZON,
            origin: None,
            stability_threshold: 0.02,
        }
    }

    /// Sets how many samples are retained.
    ///
    /// Fewer than three cannot describe a line, so the window is floored there.
    #[must_use]
    pub fn with_window(mut self, window: usize) -> Self {
        self.capacity = window.max(3);
        self
    }

    /// Sets how far ahead forecasts reach.
    #[must_use]
    pub const fn with_horizon(mut self, horizon: Duration) -> Self {
        self.horizon = horizon;
        self
    }

    /// Records a measurement taken at `now`.
    ///
    /// Non-finite values are ignored rather than corrupting the regression.
    pub fn record(&mut self, value: f64, now: Instant) {
        if !value.is_finite() {
            return;
        }

        let origin = *self.origin.get_or_insert(now);
        let elapsed = now.saturating_duration_since(origin).as_secs_f64();

        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back((elapsed, value));
    }

    /// Returns how many samples are retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Returns `true` if nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Returns the most recent value.
    #[must_use]
    pub fn current(&self) -> Option<f64> {
        self.samples.back().map(|&(_, value)| value)
    }

    /// Returns the trend direction alone.
    #[must_use]
    pub fn trend(&self, now: Instant) -> Trend {
        self.forecast(now).map_or(Trend::Stable, |f| f.trend)
    }

    /// Returns a forecast as of `now`, if enough samples exist.
    ///
    /// Returns `None` with fewer than three samples: two points always fit a
    /// line perfectly, which would report full confidence in a trend drawn
    /// through nothing.
    #[must_use]
    pub fn forecast(&self, now: Instant) -> Option<Forecast> {
        if self.samples.len() < 3 {
            return None;
        }

        let count = self.samples.len() as f64;
        let mean_x = self.samples.iter().map(|&(x, _)| x).sum::<f64>() / count;
        let mean_y = self.samples.iter().map(|&(_, y)| y).sum::<f64>() / count;

        let mut covariance = 0.0;
        let mut variance_x = 0.0;
        for &(x, y) in &self.samples {
            covariance += (x - mean_x) * (y - mean_y);
            variance_x += (x - mean_x) * (x - mean_x);
        }

        // Every sample landed at the same instant, so no slope is defined.
        if variance_x.abs() < f64::EPSILON {
            return None;
        }

        let slope = covariance / variance_x;
        let intercept = mean_y - slope * mean_x;

        let current = self.current()?;
        let origin = self.origin?;
        let elapsed_now = now.saturating_duration_since(origin).as_secs_f64();
        let target_x = elapsed_now + self.horizon.as_secs_f64();

        // A linear extrapolation happily runs below zero, which is meaningless
        // for a rate.
        let predicted = (slope * target_x + intercept).max(0.0);

        let confidence = self.fit_quality(slope, intercept, mean_y);

        let relative_slope = if mean_y.abs() > f64::EPSILON {
            slope / mean_y
        } else {
            0.0
        };

        let trend = if relative_slope > self.stability_threshold {
            Trend::Improving
        } else if relative_slope < -self.stability_threshold {
            Trend::Degrading
        } else {
            Trend::Stable
        };

        Some(Forecast {
            current,
            predicted,
            trend,
            confidence,
            horizon: self.horizon,
            slope_per_second: slope,
        })
    }

    /// Returns the coefficient of determination for the fitted line.
    fn fit_quality(&self, slope: f64, intercept: f64, mean_y: f64) -> f64 {
        let mut residual = 0.0;
        let mut total = 0.0;

        for &(x, y) in &self.samples {
            let fitted = slope * x + intercept;
            residual += (y - fitted) * (y - fitted);
            total += (y - mean_y) * (y - mean_y);
        }

        if total.abs() < f64::EPSILON {
            // Every sample identical: a flat line fits perfectly.
            return 1.0;
        }

        (1.0 - residual / total).clamp(0.0, 1.0)
    }

    /// Discards all history.
    pub fn reset(&mut self) {
        self.samples.clear();
        self.origin = None;
    }
}

impl Default for TrendPredictor {
    fn default() -> Self {
        Self::new()
    }
}

/// What a predictive scheduler suggests doing next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SendAdvice {
    /// Send now at the current rate.
    SendNow,
    /// Send now, and prefer to send more while conditions are good.
    ///
    /// Returned when capacity is expected to fall: work moved earlier is work
    /// that avoids the worse conditions ahead.
    SendAggressively,
    /// Reduce the rate; conditions are deteriorating.
    Throttle,
    /// Hold non-urgent work briefly; conditions are expected to improve.
    Defer {
        /// How long to wait before reconsidering.
        delay: Duration,
    },
}

impl SendAdvice {
    /// Returns `true` if work should be sent now.
    #[must_use]
    pub const fn should_send(self) -> bool {
        matches!(self, Self::SendNow | Self::SendAggressively)
    }

    /// Returns the suggested delay, if any.
    #[must_use]
    pub const fn delay(self) -> Option<Duration> {
        match self {
            Self::Defer { delay } => Some(delay),
            _ => None,
        }
    }
}

/// Turns a forecast into scheduling advice.
///
/// `urgent` work is never deferred: predicting better conditions shortly is no
/// reason to delay a heartbeat or a user-visible request. Only bulk traffic is
/// worth holding.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
/// use nexusnet_optimizer::{advise_send, SendAdvice, TrendPredictor};
///
/// let start = Instant::now();
/// let mut predictor = TrendPredictor::new();
///
/// // Bandwidth collapsing steadily.
/// for step in 0..12_u64 {
///     predictor.record(4000.0 - (step as f64) * 300.0, start + Duration::from_secs(step));
/// }
///
/// let forecast = predictor.forecast(start + Duration::from_secs(12)).expect("samples");
/// // Bulk work should move now, before conditions worsen further.
/// assert_eq!(advise_send(Some(forecast), false), SendAdvice::SendAggressively);
/// ```
#[must_use]
pub fn advise_send(forecast: Option<Forecast>, urgent: bool) -> SendAdvice {
    let Some(forecast) = forecast else {
        return SendAdvice::SendNow;
    };

    if !forecast.is_actionable() {
        return SendAdvice::SendNow;
    }

    match forecast.trend {
        // Capacity is disappearing: move what we can while it exists.
        Trend::Degrading => SendAdvice::SendAggressively,
        Trend::Improving if !urgent => SendAdvice::Defer {
            delay: forecast.horizon / 2,
        },
        Trend::Improving | Trend::Stable => SendAdvice::SendNow,
    }
}

/// Returns advice for a link that is also showing congestion.
///
/// Congestion overrides the forecast: a filling queue is a present fact, while
/// a trend is an inference about the future.
#[must_use]
pub fn advise_send_under_congestion(
    forecast: Option<Forecast>,
    urgent: bool,
    congested: bool,
) -> SendAdvice {
    if congested && !urgent {
        return SendAdvice::Throttle;
    }

    advise_send(forecast, urgent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start() -> Instant {
        Instant::now()
    }

    /// Feeds `count` samples one second apart, starting at `first` and changing
    /// by `step` each time.
    fn feed(predictor: &mut TrendPredictor, origin: Instant, first: f64, step: f64, count: u64) {
        for index in 0..count {
            let value = first + step * index as f64;
            predictor.record(value, origin + Duration::from_secs(index));
        }
    }

    #[test]
    fn too_few_samples_yield_no_forecast() {
        let now = start();
        let mut predictor = TrendPredictor::new();

        assert!(predictor.forecast(now).is_none());

        predictor.record(100.0, now);
        predictor.record(200.0, now + Duration::from_secs(1));

        assert!(
            predictor.forecast(now + Duration::from_secs(2)).is_none(),
            "two points always fit a line perfectly, which would be false confidence"
        );
    }

    #[test]
    fn a_falling_measurement_is_degrading() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        feed(&mut predictor, now, 2000.0, -100.0, 12);

        let forecast = predictor
            .forecast(now + Duration::from_secs(12))
            .expect("samples exist");

        assert_eq!(forecast.trend, Trend::Degrading);
        assert!(forecast.trend.is_degrading());
        assert!(forecast.predicted < forecast.current);
        assert!(forecast.slope_per_second < 0.0);
    }

    #[test]
    fn a_rising_measurement_is_improving() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        feed(&mut predictor, now, 1000.0, 200.0, 12);

        let forecast = predictor
            .forecast(now + Duration::from_secs(12))
            .expect("samples exist");

        assert_eq!(forecast.trend, Trend::Improving);
        assert!(forecast.predicted > forecast.current);
    }

    #[test]
    fn a_flat_measurement_is_stable() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        feed(&mut predictor, now, 1500.0, 0.0, 12);

        let forecast = predictor
            .forecast(now + Duration::from_secs(12))
            .expect("samples exist");

        assert_eq!(forecast.trend, Trend::Stable);
        assert!((forecast.predicted - 1500.0).abs() < 1.0);
    }

    #[test]
    fn a_clean_trend_reports_high_confidence() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        feed(&mut predictor, now, 1000.0, 100.0, 12);

        let forecast = predictor
            .forecast(now + Duration::from_secs(12))
            .expect("samples exist");

        assert!(
            forecast.confidence > 0.95,
            "a perfectly linear series should fit almost exactly, got {}",
            forecast.confidence
        );
        assert!(forecast.is_actionable());
    }

    #[test]
    fn noise_reports_low_confidence() {
        let now = start();
        let mut predictor = TrendPredictor::new();

        // Alternating values with no underlying direction.
        for index in 0..16_u64 {
            let value = if index % 2 == 0 { 500.0 } else { 2500.0 };
            predictor.record(value, now + Duration::from_secs(index));
        }

        let forecast = predictor
            .forecast(now + Duration::from_secs(16))
            .expect("samples exist");

        assert!(
            forecast.confidence < 0.5,
            "noise around a level is not a trend, got confidence {}",
            forecast.confidence
        );
        assert!(
            !forecast.is_actionable(),
            "acting on a line drawn through noise is worse than not predicting"
        );
    }

    #[test]
    fn predictions_never_go_negative() {
        let now = start();
        let mut predictor = TrendPredictor::new().with_horizon(Duration::from_secs(60));
        // Falling fast enough that a straight line would cross zero.
        feed(&mut predictor, now, 1000.0, -100.0, 10);

        let forecast = predictor
            .forecast(now + Duration::from_secs(10))
            .expect("samples exist");

        assert!(
            forecast.predicted >= 0.0,
            "negative bandwidth is meaningless, got {}",
            forecast.predicted
        );
    }

    #[test]
    fn the_window_is_bounded() {
        let now = start();
        let mut predictor = TrendPredictor::new().with_window(5);
        feed(&mut predictor, now, 100.0, 10.0, 100);

        assert_eq!(predictor.len(), 5, "old samples must be discarded");
    }

    #[test]
    fn a_tiny_window_is_floored() {
        let predictor = TrendPredictor::new().with_window(1);
        assert!(
            predictor.capacity >= 3,
            "fewer than three cannot fit a line"
        );
    }

    #[test]
    fn the_window_lets_the_trend_reverse() {
        let now = start();
        let mut predictor = TrendPredictor::new().with_window(8);

        // Rising, then falling.
        feed(&mut predictor, now, 1000.0, 200.0, 10);
        for index in 10..24_u64 {
            let value = 3000.0 - (index - 10) as f64 * 200.0;
            predictor.record(value, now + Duration::from_secs(index));
        }

        let forecast = predictor
            .forecast(now + Duration::from_secs(24))
            .expect("samples exist");

        assert_eq!(
            forecast.trend,
            Trend::Degrading,
            "a bounded window must let the trend reverse"
        );
    }

    #[test]
    fn relative_change_is_proportional() {
        let now = start();
        let mut predictor = TrendPredictor::new().with_horizon(Duration::from_secs(5));
        feed(&mut predictor, now, 1000.0, -50.0, 12);

        let forecast = predictor
            .forecast(now + Duration::from_secs(12))
            .expect("samples exist");

        assert!(
            forecast.relative_change() < 0.0,
            "a falling series should predict a proportional decrease"
        );
    }

    #[test]
    fn degenerate_samples_are_ignored() {
        let now = start();
        let mut predictor = TrendPredictor::new();

        predictor.record(f64::NAN, now);
        predictor.record(f64::INFINITY, now + Duration::from_secs(1));

        assert!(predictor.is_empty());
    }

    #[test]
    fn samples_at_one_instant_yield_no_slope() {
        let now = start();
        let mut predictor = TrendPredictor::new();

        for _ in 0..10 {
            predictor.record(100.0, now);
        }

        assert!(
            predictor.forecast(now).is_none(),
            "no time has elapsed, so no rate of change is defined"
        );
    }

    #[test]
    fn resetting_clears_history() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        feed(&mut predictor, now, 1000.0, 100.0, 12);

        predictor.reset();
        assert!(predictor.is_empty());
        assert!(predictor.forecast(now).is_none());
    }

    #[test]
    fn no_forecast_means_send_normally() {
        assert_eq!(advise_send(None, false), SendAdvice::SendNow);
        assert!(advise_send(None, false).should_send());
    }

    #[test]
    fn degrading_conditions_move_work_earlier() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        feed(&mut predictor, now, 4000.0, -300.0, 12);

        let forecast = predictor.forecast(now + Duration::from_secs(12));

        assert_eq!(
            advise_send(forecast, false),
            SendAdvice::SendAggressively,
            "capacity is disappearing, so use it while it exists"
        );
    }

    #[test]
    fn improving_conditions_defer_bulk_work() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        feed(&mut predictor, now, 1000.0, 400.0, 12);

        let forecast = predictor.forecast(now + Duration::from_secs(12));
        let advice = advise_send(forecast, false);

        assert!(
            matches!(advice, SendAdvice::Defer { .. }),
            "bulk work can wait for better conditions, got {advice:?}"
        );
        assert!(advice.delay().is_some());
        assert!(!advice.should_send());
    }

    #[test]
    fn urgent_work_is_never_deferred() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        feed(&mut predictor, now, 1000.0, 400.0, 12);

        let forecast = predictor.forecast(now + Duration::from_secs(12));

        assert_eq!(
            advise_send(forecast, true),
            SendAdvice::SendNow,
            "a heartbeat does not wait for a better moment"
        );
    }

    #[test]
    fn an_unactionable_forecast_changes_nothing() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        // A barely perceptible drift.
        feed(&mut predictor, now, 1000.0, -1.0, 12);

        let forecast = predictor.forecast(now + Duration::from_secs(12));
        assert_eq!(advise_send(forecast, false), SendAdvice::SendNow);
    }

    #[test]
    fn congestion_overrides_the_forecast() {
        let now = start();
        let mut predictor = TrendPredictor::new();
        feed(&mut predictor, now, 4000.0, -300.0, 12);

        let forecast = predictor.forecast(now + Duration::from_secs(12));

        assert_eq!(
            advise_send_under_congestion(forecast, false, true),
            SendAdvice::Throttle,
            "a filling queue is a present fact; a trend is an inference"
        );
        assert_eq!(
            advise_send_under_congestion(forecast, true, true),
            SendAdvice::SendAggressively,
            "but urgent work still goes out"
        );
    }

    #[test]
    fn trends_display() {
        assert_eq!(Trend::Degrading.to_string(), "degrading");
        assert!(Trend::Improving.is_improving());
    }
}
