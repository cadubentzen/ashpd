use std::os::{fd::BorrowedFd, unix::io::AsRawFd};

use gst::prelude::*;
use gtk::glib;

/// A self-contained GStreamer pipeline that muxes the captured video (VP9) and,
/// optionally, audio (Opus) into a single webm file.
///
/// It is decoupled from the display paintables: it opens its own `pipewiresrc`
/// elements on the shared PipeWire fd (the same fd is already shared across the
/// display paintables, so this is consistent with existing usage).
#[derive(Debug)]
pub struct ScreenCastRecorder {
    pipeline: gst::Pipeline,
    guard: gst::bus::BusWatchGuard,
}

impl ScreenCastRecorder {
    /// Build and start a recording pipeline writing webm to `path`.
    ///
    /// `audio_node` is `None` when no audio stream is captured, in which case the
    /// resulting webm contains only the VP9 video track.
    pub fn new(
        fd: BorrowedFd<'_>,
        video_node: u32,
        audio_node: Option<u32>,
        path: &str,
    ) -> anyhow::Result<Self> {
        tracing::debug!("Init recorder pipeline -> {path}");
        let raw_fd = fd.as_raw_fd();
        let pipeline = gst::Pipeline::new();

        let webmmux = gst::ElementFactory::make("webmmux").build()?;
        let filesink = gst::ElementFactory::make("filesink").build()?;
        filesink.set_property("location", path);
        pipeline.add_many([&webmmux, &filesink])?;
        webmmux.link(&filesink)?;

        // Video branch: own pipewiresrc on the recorded node -> VP9.
        let video_src = gst::ElementFactory::make("pipewiresrc").build()?;
        video_src.set_property("fd", raw_fd);
        video_src.set_property("path", video_node.to_string());
        let video_queue = gst::ElementFactory::make("queue").build()?;
        let videoconvert = gst::ElementFactory::make("videoconvert").build()?;
        let vp9enc = gst::ElementFactory::make("vp9enc").build()?;
        // libvpx realtime mode so software encoding keeps up with the live stream.
        vp9enc.set_property("deadline", 1i64);
        let video_enc_queue = gst::ElementFactory::make("queue").build()?;
        pipeline.add_many([
            &video_src,
            &video_queue,
            &videoconvert,
            &vp9enc,
            &video_enc_queue,
        ])?;
        gst::Element::link_many([
            &video_src,
            &video_queue,
            &videoconvert,
            &vp9enc,
            &video_enc_queue,
        ])?;
        video_enc_queue.link(&webmmux)?;

        // Audio branch (optional): own pipewiresrc on the audio node -> Opus.
        if let Some(audio_node) = audio_node {
            let audio_src = gst::ElementFactory::make("pipewiresrc").build()?;
            audio_src.set_property("fd", raw_fd);
            audio_src.set_property("path", audio_node.to_string());
            let audio_queue = gst::ElementFactory::make("queue").build()?;
            let audioconvert = gst::ElementFactory::make("audioconvert").build()?;
            let audioresample = gst::ElementFactory::make("audioresample").build()?;
            let opusenc = gst::ElementFactory::make("opusenc").build()?;
            pipeline.add_many([
                &audio_src,
                &audio_queue,
                &audioconvert,
                &audioresample,
                &opusenc,
            ])?;
            gst::Element::link_many([
                &audio_src,
                &audio_queue,
                &audioconvert,
                &audioresample,
                &opusenc,
            ])?;
            opusenc.link(&webmmux)?;
        }

        let bus = pipeline.bus().unwrap();
        let guard = bus
            .add_watch_local(move |_, msg| {
                if let gst::MessageView::Error(err) = msg.view() {
                    tracing::error!(
                        "Recorder error from {:?}: {} ({:?})",
                        err.src().map(|s| s.path_string()),
                        err.error(),
                        err.debug()
                    );
                }
                glib::ControlFlow::Continue
            })
            .expect("Failed to add recorder bus watch");

        pipeline.set_state(gst::State::Playing)?;
        Ok(Self { pipeline, guard })
    }

    /// Finalize the webm: drop the bus watch, send EOS, wait (bounded ~3s) for the
    /// muxer to flush, then set the pipeline to Null.
    pub fn stop(self) {
        tracing::debug!("Stopping recorder");
        // Drop the bus watch first so it doesn't consume the EOS we wait for below.
        drop(self.guard);
        if self.pipeline.send_event(gst::event::Eos::new()) {
            let finalized = self
                .pipeline
                .bus()
                .and_then(|bus| {
                    bus.timed_pop_filtered(
                        gst::ClockTime::from_seconds(3),
                        &[gst::MessageType::Eos, gst::MessageType::Error],
                    )
                })
                .is_some();
            if !finalized {
                tracing::warn!("Timed out waiting for EOS; recording may be truncated");
            }
        } else {
            tracing::warn!("Failed to send EOS; recording may be truncated");
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
