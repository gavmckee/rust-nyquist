mod slice;
pub use slice::{HistogramSlice, empty_accumulator, percentile};

mod sliding;
pub use sliding::SlidingHistogram;
