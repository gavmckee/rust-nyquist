mod slice;
pub use slice::{
    HistogramSlice, empty_accumulator, percentile,
    DEFAULT_SAMPLES_PER_SLICE, HIST_GROUPING_POWER, HIST_MAX_TRACKABLE, HIST_MAX_VALUE_POWER,
};

mod sliding;
pub use sliding::SlidingHistogram;
