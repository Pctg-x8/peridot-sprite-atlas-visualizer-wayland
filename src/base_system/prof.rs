//! mini profiler

use std::{
    io::{IoSlice, Write},
    path::Path,
};

use shared_perflog_proto::{ProfileMarker, ProfileMarkerCategory};

pub struct ScopedMarker<'p, 'f> {
    ctx: &'f mut ProfilingFrameContext<'p>,
    marker: ProfileMarker,
}
impl Drop for ScopedMarker<'_, '_> {
    #[inline(always)]
    fn drop(&mut self) {
        self.ctx
            .ctx
            .append_now(self.marker, ProfileMarkerCategory::End);
    }
}

pub struct ProfilingFrameContext<'p> {
    ctx: &'p mut ProfilingContext,
}
impl Drop for ProfilingFrameContext<'_> {
    fn drop(&mut self) {
        self.ctx
            .append_now(ProfileMarker::Frame, ProfileMarkerCategory::End);
    }
}
impl<'p> ProfilingFrameContext<'p> {
    #[inline(always)]
    pub fn scoped<'f>(&'f mut self, marker: ProfileMarker) -> ScopedMarker<'p, 'f> {
        self.ctx.append_now(marker, ProfileMarkerCategory::Begin);
        ScopedMarker { ctx: self, marker }
    }

    #[inline(always)]
    pub fn record(&mut self, marker: ProfileMarker, cat: ProfileMarkerCategory) {
        self.ctx.append_now(marker, cat);
    }

    #[inline(always)]
    pub fn begin_resize(&mut self) {
        self.ctx
            .append_now(ProfileMarker::Resize, ProfileMarkerCategory::Begin);
    }

    #[inline(always)]
    pub fn end_resize(&mut self) {
        self.ctx
            .append_now(ProfileMarker::Resize, ProfileMarkerCategory::End);
    }

    #[inline(always)]
    pub fn begin_populate_composite_instances(&mut self) {
        self.ctx.append_now(
            ProfileMarker::PopulateCompositeInstances,
            ProfileMarkerCategory::Begin,
        );
    }

    #[inline(always)]
    pub fn end_populate_composite_instances(&mut self) {
        self.ctx.append_now(
            ProfileMarker::PopulateCompositeInstances,
            ProfileMarkerCategory::End,
        );
    }
}

pub struct ProfilingContext {
    #[cfg(feature = "profiling")]
    fp: std::io::BufWriter<std::fs::File>,
    #[cfg(feature = "profiling")]
    last_frame_index: u32,
}
impl ProfilingContext {
    #[cfg(feature = "profiling")]
    const BUFFERING_SIZE: usize = 8192;

    #[cfg(feature = "profiling")]
    pub fn init(output_path: impl AsRef<Path>) -> Self {
        let mut fp = std::fs::File::options()
            .create(true)
            .truncate(true)
            .write(true)
            .open(output_path)
            .unwrap();
        shared_perflog_proto::write_file_head(&mut fp, Self::timestamp_freq()).unwrap();

        Self {
            fp: std::io::BufWriter::with_capacity(Self::BUFFERING_SIZE, fp),
            last_frame_index: 0,
        }
    }

    #[cfg(not(feature = "profiling"))]
    pub fn init(_output_path: impl AsRef<Path>) -> Self {
        Self {}
    }

    #[inline]
    pub fn begin_frame<'p>(&'p mut self) -> ProfilingFrameContext<'p> {
        #[cfg(feature = "profiling")]
        {
            let ts = Self::timestamp();
            self.last_frame_index += 1;
            let fx = self.last_frame_index;
            let ctx = ProfilingFrameContext { ctx: self };

            // write begin frame sample
            if let Err(e) = shared_perflog_proto::serialize_begin_frame(&mut ctx.ctx.fp, ts, fx) {
                tracing::warn!(reason = ?e, "write begin frame failed");
            }

            ctx
        }
    }

    #[inline(always)]
    fn append_now(&mut self, marker: ProfileMarker, cat: ProfileMarkerCategory) {
        #[cfg(feature = "profiling")]
        if let Err(e) =
            shared_perflog_proto::write_sample_head(&mut self.fp, marker, cat, Self::timestamp())
        {
            tracing::warn!(reason = ?e, "write perflog sample failed");
        }
    }

    pub fn flush(&mut self) {
        if let Err(e) = self.fp.flush() {
            tracing::warn!(reason = ?e, "flush perflog failed");
        }
    }

    pub fn timestamp() -> u64 {
        #[cfg(target_os = "linux")]
        {
            crate::platform::linux::time::hires_tick()
        }
    }

    pub fn timestamp_freq() -> u64 {
        #[cfg(target_os = "linux")]
        {
            crate::platform::linux::time::hires_tick_freq()
        }
    }
}
