mod slice;
pub use slice::{HistogramSlice, empty_accumulator, percentile, HIST_GROUPING_POWER, HIST_MAX_VALUE_POWER};

mod sliding;
pub use sliding::SlidingHistogram;
