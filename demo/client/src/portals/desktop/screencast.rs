use std::{
    cell::RefCell,
    os::fd::{AsFd, OwnedFd},
    sync::Arc,
};

use adw::{prelude::*, subclass::prelude::*};
use ashpd::{
    WindowIdentifier,
    desktop::{
        PersistMode, Session,
        screencast::{
            AudioOptions, CursorMode, MediaType, Screencast, SelectSourcesOptions, SourceType,
            Stream,
        },
    },
    enumflags2::BitFlags,
};
use futures_util::lock::Mutex;
use gtk::glib::{self, clone};

use crate::{
    portals::spawn_tokio,
    widgets::{
        CameraPaintable, NoiseGenerator, PortalPage, PortalPageExt, PortalPageImpl,
        ScreenCastRecorder,
    },
};

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/com/belmoussaoui/ashpd/demo/screencast.ui")]
    pub struct ScreenCastPage {
        #[template_child]
        pub streams_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub response_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub multiple_switch: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub audio_switch: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub include_self_switch: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub record_switch: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub record_path_entry: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub noise_switch: TemplateChild<adw::SwitchRow>,
        pub session: Arc<Mutex<Option<Session<Screencast>>>>,
        #[template_child]
        pub monitor_check: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub window_check: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub virtual_check: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub cursor_mode_combo: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub persist_mode_combo: TemplateChild<adw::ComboRow>,
        pub session_token: Arc<Mutex<Option<String>>>,
        pub recorder: RefCell<Option<ScreenCastRecorder>>,
        pub noise: RefCell<Option<NoiseGenerator>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ScreenCastPage {
        const NAME: &'static str = "ScreenCastPage";
        type Type = super::ScreenCastPage;
        type ParentType = PortalPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();

            klass.install_action_async("screencast.start", None, |page, _, _| async move {
                page.start_session().await;
            });
            klass.install_action_async("screencast.stop", None, |page, _, _| async move {
                page.stop_session().await;
                page.info("Screen cast session stopped");
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }
    impl ObjectImpl for ScreenCastPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().action_set_enabled("screencast.stop", false);

            // The recording path is only editable when recording is enabled.
            self.record_switch
                .bind_property("active", &*self.record_path_entry, "sensitive")
                .sync_create()
                .build();

            // Include-own-audio is only meaningful when audio is requested.
            self.audio_switch
                .bind_property("active", &*self.include_self_switch, "sensitive")
                .sync_create()
                .build();

            // Start/stop the white-noise test source the moment the switch is
            // toggled, independent of the screen cast session.
            let page = self.obj();
            self.noise_switch.connect_active_notify(clone!(
                #[weak]
                page,
                move |switch| {
                    let imp = page.imp();
                    if switch.is_active() {
                        match NoiseGenerator::new() {
                            Ok(noise) => {
                                imp.noise.replace(Some(noise));
                                page.info("Producing pink-noise test audio");
                            }
                            Err(err) => {
                                tracing::error!("Failed to start noise generator: {err}");
                                page.error(&format!("Failed to produce test audio: {err}"));
                                switch.set_active(false);
                            }
                        }
                    } else if let Some(noise) = imp.noise.take() {
                        noise.stop();
                    }
                }
            ));
        }
    }
    impl WidgetImpl for ScreenCastPage {
        fn map(&self) {
            let widget = self.obj();
            glib::spawn_future_local(clone!(
                #[weak]
                widget,
                async move {
                    let imp = widget.imp();
                    if let Ok((cursor_modes, source_types)) = available_types().await {
                        imp.virtual_check
                            .set_sensitive(source_types.contains(SourceType::Virtual));
                        imp.monitor_check
                            .set_sensitive(source_types.contains(SourceType::Monitor));
                        imp.window_check
                            .set_sensitive(source_types.contains(SourceType::Window));
                        let model = gtk::StringList::default();
                        if cursor_modes.contains(CursorMode::Hidden) {
                            model.append("Hidden");
                        }
                        if cursor_modes.contains(CursorMode::Metadata) {
                            model.append("Metadata");
                        }
                        if cursor_modes.contains(CursorMode::Embedded) {
                            model.append("Embedded");
                        }
                        imp.cursor_mode_combo.set_model(Some(&model));
                    }
                }
            ));

            widget.set_property(
                "portal-docs-url",
                "https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html",
            );

            glib::spawn_future_local(glib::clone!(
                #[weak]
                widget,
                async move {
                    if let Ok(proxy) = spawn_tokio(async { Screencast::new().await }).await {
                        widget.set_property("portal-version", proxy.version());
                    }
                }
            ));
            self.parent_map();
        }
    }
    impl BinImpl for ScreenCastPage {}
    impl PortalPageImpl for ScreenCastPage {}
}

glib::wrapper! {
    pub struct ScreenCastPage(ObjectSubclass<imp::ScreenCastPage>)
        @extends gtk::Widget, adw::Bin, PortalPage,
        @implements gtk::ConstraintTarget, gtk::Buildable, gtk::Accessible;
}

/// The stream metadata needed to associate an audio stream with its source.
trait StreamMapping {
    fn is_audio(&self) -> bool;
    fn mapping_id(&self) -> Option<&str>;
}

impl StreamMapping for Stream {
    fn is_audio(&self) -> bool {
        self.media_type() == MediaType::Audio
    }

    fn mapping_id(&self) -> Option<&str> {
        Stream::mapping_id(self)
    }
}

/// Returns the index of the single audio stream that shares `video`'s
/// non-empty mapping id, or `None` when the mapping is missing, empty,
/// matches no audio, or is ambiguous.
fn unique_mapped_audio<T: StreamMapping>(streams: &[T], video: &T) -> Option<usize> {
    let mapping_id = video.mapping_id().filter(|id| !id.is_empty())?;

    let video_count = streams
        .iter()
        .filter(|s| !s.is_audio() && s.mapping_id() == Some(mapping_id))
        .count();
    if video_count != 1 {
        return None;
    }

    let mut matches = streams
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_audio() && s.mapping_id() == Some(mapping_id))
        .map(|(index, _)| index);
    let index = matches.next()?;
    matches.next().is_none().then_some(index)
}

impl ScreenCastPage {
    /// Returns the selected SourceType
    fn selected_sources(&self) -> BitFlags<SourceType> {
        let imp = self.imp();
        let mut sources: BitFlags<SourceType> = BitFlags::empty();
        if imp.monitor_check.is_active() {
            sources.insert(SourceType::Monitor);
        }
        if imp.window_check.is_active() {
            sources.insert(SourceType::Window);
        }
        if imp.virtual_check.is_active() {
            sources.insert(SourceType::Virtual);
        }
        sources
    }

    /// Returns the selected CursorMode
    fn selected_cursor_mode(&self) -> CursorMode {
        match self
            .imp()
            .cursor_mode_combo
            .selected_item()
            .and_downcast::<gtk::StringObject>()
            .unwrap()
            .string()
            .as_ref()
        {
            "Hidden" => CursorMode::Hidden,
            "Embedded" => CursorMode::Embedded,
            "Metadata" => CursorMode::Metadata,
            _ => unreachable!(),
        }
    }

    fn selected_persist_mode(&self) -> PersistMode {
        match self.imp().persist_mode_combo.selected() {
            0 => PersistMode::DoNot,
            1 => PersistMode::Application,
            2 => PersistMode::ExplicitlyRevoked,
            _ => unreachable!(),
        }
    }

    async fn start_session(&self) {
        let imp = self.imp();
        self.action_set_enabled("screencast.start", false);
        self.action_set_enabled("screencast.stop", true);

        match self.screencast().await {
            Ok((streams, fd, session)) => {
                self.success("Screen cast session started successfully");

                // Record whenever the Record switch is on, regardless of Audio. An empty
                // path field falls back to the default webm location.
                let record_path = imp.record_switch.is_active().then(|| {
                    let text = imp.record_path_entry.text().to_string();
                    if text.is_empty() {
                        "/tmp/ashpd.webm".to_string()
                    } else {
                        text
                    }
                });

                // The webm records the first video stream plus the audio stream (if any).
                let video_index = streams
                    .iter()
                    .position(|s| s.media_type() != MediaType::Audio);
                let video_node = video_index.map(|index| streams[index].pipe_wire_node_id());
                let audio_node = video_index.and_then(|index| {
                    unique_mapped_audio(&streams, &streams[index])
                        .map(|audio_index| streams[audio_index].pipe_wire_node_id())
                });

                streams.iter().for_each(|stream: &Stream| {
                    let paintable = CameraPaintable::default();
                    let picture = gtk::Picture::builder()
                        .paintable(&paintable)
                        .width_request(480)
                        .vexpand(true)
                        .build();
                    if stream.media_type() == MediaType::Audio {
                        self.info(&format!(
                            "Audio stream available at PipeWire node {}",
                            stream.pipe_wire_node_id()
                        ));
                        paintable.set_audio_pipewire_node_id(
                            fd.as_fd(),
                            Some(stream.pipe_wire_node_id()),
                        );
                    } else {
                        paintable
                            .set_pipewire_node_id(fd.as_fd(), Some(stream.pipe_wire_node_id()));
                    }
                    imp.streams_box.append(&picture);
                });

                if let Some(ref path) = record_path {
                    let video_count = streams
                        .iter()
                        .filter(|s| s.media_type() != MediaType::Audio)
                        .count();
                    if video_count > 1 {
                        tracing::info!(
                            "Recording first video stream only; {} other video stream(s) not recorded",
                            video_count - 1
                        );
                    }
                    match video_node {
                        Some(video_node) => {
                            match ScreenCastRecorder::new(
                                fd.as_fd(),
                                video_node,
                                audio_node,
                                path,
                            ) {
                                Ok(recorder) => {
                                    self.info(&format!("Recording to {path}"));
                                    imp.recorder.replace(Some(recorder));
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        "Recording unavailable ({err}); continuing without recording"
                                    );
                                }
                            }
                        }
                        None => {
                            tracing::warn!("No video stream to record; continuing without recording");
                        }
                    }
                }

                imp.response_group.set_visible(true);

                if let Some(old_session) = imp.session.lock().await.replace(session) {
                    spawn_tokio(async move {
                        let _ = old_session.close().await;
                    })
                    .await;
                }
            }
            Err(err) => {
                tracing::error!("Failed to start screen cast session: {err}");
                self.error(&format!("Failed to start a screen cast session: {err}"));
                self.stop_session().await;
            }
        };
    }

    async fn stop_session(&self) {
        let imp = self.imp();
        if let Some(recorder) = imp.recorder.take() {
            recorder.stop();
        }

        self.action_set_enabled("screencast.start", true);
        self.action_set_enabled("screencast.stop", false);

        if let Some(session) = imp.session.lock().await.take() {
            spawn_tokio(async move {
                let _ = session.close().await;
            })
            .await;
        }
        while let Some(child) = imp.streams_box.first_child() {
            let picture = child.downcast_ref::<gtk::Picture>().unwrap();
            let paintable = picture
                .paintable()
                .and_downcast::<CameraPaintable>()
                .unwrap();
            paintable.close_pipeline();
            imp.streams_box.remove(picture);
        }

        imp.response_group.set_visible(false);
    }

    async fn screencast(&self) -> ashpd::Result<(Vec<Stream>, OwnedFd, Session<Screencast>)> {
        let imp = self.imp();
        let sources = self.selected_sources();
        let cursor_mode = self.selected_cursor_mode();
        let persist_mode = self.selected_persist_mode();
        let multiple = imp.multiple_switch.is_active();
        let audio = imp.audio_switch.is_active().then(|| {
            AudioOptions::default().set_include_self(imp.include_self_switch.is_active())
        });

        let root = self.native().unwrap();

        let identifier = WindowIdentifier::from_native(&root).await;
        let prev_token = imp
            .session_token
            .lock()
            .await
            .as_deref()
            .map(ToOwned::to_owned);

        self.info("Starting a screen cast session");
        let (streams, fd, session, new_token) = spawn_tokio(async move {
            let proxy = Screencast::new().await?;
            let session = proxy.create_session(Default::default()).await?;
            proxy
                .select_sources(
                    &session,
                    SelectSourcesOptions::default()
                        .set_cursor_mode(cursor_mode)
                        .set_sources(sources)
                        .set_multiple(multiple)
                        .set_audio(audio)
                        .set_restore_token(prev_token.as_deref())
                        .set_persist_mode(persist_mode),
                )
                .await?;
            let response = proxy
                .start(&session, identifier.as_ref(), Default::default())
                .await?
                .response()?;

            let fd = proxy
                .open_pipe_wire_remote(&session, Default::default())
                .await?;
            ashpd::Result::Ok((
                response.streams().to_owned(),
                fd,
                session,
                response.restore_token().map(ToOwned::to_owned),
            ))
        })
        .await?;
        if let Some(t) = new_token {
            imp.session_token.lock().await.replace(t.to_owned());
        }
        Ok((streams, fd, session))
    }
}

pub async fn available_types() -> ashpd::Result<(BitFlags<CursorMode>, BitFlags<SourceType>)> {
    spawn_tokio(async move {
        let proxy = Screencast::new().await?;

        let cursor_modes = proxy.available_cursor_modes().await?;
        let source_types = proxy.available_source_types().await?;

        ashpd::Result::Ok((cursor_modes, source_types))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct FakeStream {
        audio: bool,
        mapping_id: Option<&'static str>,
    }

    impl StreamMapping for FakeStream {
        fn is_audio(&self) -> bool {
            self.audio
        }

        fn mapping_id(&self) -> Option<&str> {
            self.mapping_id
        }
    }

    fn stream(audio: bool, mapping_id: Option<&'static str>) -> FakeStream {
        FakeStream { audio, mapping_id }
    }

    #[test]
    fn unique_mapped_audio_association() {
        // Complete group in arbitrary order.
        let streams = vec![
            stream(true, Some("m0")),
            stream(false, Some("m1")),
            stream(false, Some("m0")),
        ];
        assert_eq!(unique_mapped_audio(&streams, &streams[2]), Some(0));

        // Video-only group.
        let streams = vec![stream(false, Some("m0"))];
        assert_eq!(unique_mapped_audio(&streams, &streams[0]), None);

        // Unmatched audio.
        let streams = vec![stream(false, Some("m0")), stream(true, Some("m1"))];
        assert_eq!(unique_mapped_audio(&streams, &streams[0]), None);

        // Audio mapping matches multiple videos.
        let streams = vec![
            stream(false, Some("m0")),
            stream(false, Some("m0")),
            stream(true, Some("m0")),
        ];
        assert_eq!(unique_mapped_audio(&streams, &streams[0]), None);

        // Multiple audio streams for one mapping.
        let streams = vec![
            stream(false, Some("m0")),
            stream(true, Some("m0")),
            stream(true, Some("m0")),
        ];
        assert_eq!(unique_mapped_audio(&streams, &streams[0]), None);

        // Empty mapping id is never associated.
        let streams = vec![stream(false, Some("")), stream(true, Some(""))];
        assert_eq!(unique_mapped_audio(&streams, &streams[0]), None);
    }
}
