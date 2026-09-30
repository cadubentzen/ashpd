use gst::prelude::*;
use gtk::glib;

/// A self-contained GStreamer pipeline that plays low-volume pink noise to the
/// default audio output.
///
/// Its purpose is to give `ashpd-demo` an audio stream of its own so the
/// "exclude the requester's own audio" behavior of monitor audio sharing can be
/// verified by ear (and in the recorded webm): with this running, capture a
/// monitor and confirm the noise is absent from the captured stream.
#[derive(Debug)]
pub struct NoiseGenerator {
    pipeline: gst::Pipeline,
    // Held for its Drop side effect (removes the bus watch); never read directly.
    _guard: gst::bus::BusWatchGuard,
}

impl NoiseGenerator {
    /// Build and start the noise pipeline:
    ///
    /// ```text
    /// audiotestsrc wave=pink-noise ! volume volume=0.01 ! audioconvert ! audioresample ! autoaudiosink
    /// ```
    ///
    /// `autoaudiosink` routes to the default output, so PipeWire sees a playback
    /// stream owned by `ashpd-demo` (the portal requester).
    pub fn new() -> anyhow::Result<Self> {
        tracing::debug!("Init noise generator pipeline");
        let pipeline = gst::Pipeline::new();

        let src = gst::ElementFactory::make("audiotestsrc").build()?;
        // Pink noise is softer/less harsh than white noise for prolonged testing.
        src.set_property_from_str("wave", "pink-noise");
        let volume = gst::ElementFactory::make("volume").build()?;
        // Keep the noise quiet — audible enough to verify, but not jarring.
        volume.set_property("volume", 0.01f64);
        let audioconvert = gst::ElementFactory::make("audioconvert").build()?;
        let audioresample = gst::ElementFactory::make("audioresample").build()?;
        let sink = gst::ElementFactory::make("autoaudiosink").build()?;

        pipeline.add_many([&src, &volume, &audioconvert, &audioresample, &sink])?;
        gst::Element::link_many([&src, &volume, &audioconvert, &audioresample, &sink])?;

        let bus = pipeline.bus().unwrap();
        let guard = bus
            .add_watch_local(move |_, msg| {
                if let gst::MessageView::Error(err) = msg.view() {
                    tracing::error!(
                        "Noise generator error from {:?}: {} ({:?})",
                        err.src().map(|s| s.path_string()),
                        err.error(),
                        err.debug()
                    );
                }
                glib::ControlFlow::Continue
            })
            .expect("Failed to add noise generator bus watch");

        pipeline.set_state(gst::State::Playing)?;
        Ok(Self {
            pipeline,
            _guard: guard,
        })
    }

    /// Stop the noise. `self` is dropped here, which runs [`Drop`] (sets the
    /// pipeline to Null and removes the bus watch); this is just a readable
    /// call site.
    pub fn stop(self) {
        tracing::debug!("Stopping noise generator");
    }
}

impl Drop for NoiseGenerator {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
