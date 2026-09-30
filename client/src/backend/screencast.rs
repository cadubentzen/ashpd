use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use enumflags2::BitFlags;
use serde::{Deserialize, Serialize};

use crate::{
    MaybeAppID, PortalError, WindowIdentifierType,
    backend::{
        Result,
        request::{Request, RequestImpl},
        session::{CreateSessionResponse, Session, SessionImpl, SessionManager},
    },
    desktop::{
        CreateSessionOptions, HandleToken, PersistMode,
        request::Response,
        screencast::{CursorMode, SourceType, StartCastOptions},
    },
    zvariant::{self, Optional, OwnedObjectPath, OwnedValue, Type, as_value::{self, optional}},
};

pub use crate::desktop::screencast::{Stream, StreamBuilder};

#[derive(Serialize, Type, Debug, Default)]
#[zvariant(signature = "dict")]
pub struct SelectSourcesResponse {}

/// The implementation-interface options for a
/// [`ScreencastImpl::select_sources`] request.
#[derive(Serialize, Deserialize, Type, Debug, Default, Clone)]
#[zvariant(signature = "dict")]
pub struct SelectSourcesOptions {
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    types: Option<BitFlags<SourceType>>,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    multiple: Option<bool>,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    audio: Option<bool>,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    cursor_mode: Option<CursorMode>,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    restore_data: Option<(String, u32, OwnedValue)>,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    persist_mode: Option<PersistMode>,
}

impl SelectSourcesOptions {
    /// Gets the types of content to record.
    pub fn sources(&self) -> Option<BitFlags<SourceType>> {
        self.types
    }

    /// Gets whether to allow selecting multiple sources.
    pub fn is_multiple(&self) -> Option<bool> {
        self.multiple
    }

    /// Gets whether the frontend requested audio.
    ///
    /// The absence of this option is equivalent to `false`.
    pub fn is_audio(&self) -> Option<bool> {
        self.audio
    }

    /// Gets the requested cursor mode.
    pub fn cursor_mode(&self) -> Option<CursorMode> {
        self.cursor_mode
    }

    /// Gets the restore data, if any.
    pub fn restore_data<'a>(&'a self) -> Option<(&'a str, u32, &'a zvariant::Value<'a>)> {
        use std::borrow::Borrow;
        match &self.restore_data {
            Some((key, version, data)) => Some((key.as_str(), *version, data.borrow())),
            None => None,
        }
    }

    /// Gets the persist mode.
    pub fn persist_mode(&self) -> Option<PersistMode> {
        self.persist_mode
    }
}

/// An implementation-only audio descriptor for one approved selected source.
///
/// This is not a PipeWire stream: it carries no node ID. The portal frontend
/// strips and validates these before creating the real audio streams.
#[derive(Serialize, Deserialize, Type, Debug, Clone, PartialEq, Eq)]
#[zvariant(signature = "dict")]
pub struct AudioStreamDescriptor {
    #[serde(with = "as_value")]
    mapping_id: String,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    identifier: Option<AudioStreamIdentifier>,
}

impl AudioStreamDescriptor {
    /// The `mapping_id` of the video stream this audio belongs to.
    pub fn mapping_id(&self) -> &str {
        &self.mapping_id
    }
}

#[derive(Serialize, Deserialize, Type, Debug, Clone, Copy, PartialEq, Eq)]
#[zvariant(signature = "s")]
#[serde(rename_all = "lowercase")]
enum AudioStreamIdentifierType {
    Pid,
    App,
}

#[derive(Serialize, Deserialize, Type, Debug, Clone, PartialEq, Eq)]
#[zvariant(signature = "dict")]
struct AudioStreamIdentifier {
    #[serde(rename = "type", with = "as_value")]
    type_: AudioStreamIdentifierType,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    app_id: Option<String>,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    instance_id: Option<String>,
}

/// A [builder-pattern] type to construct an [`AudioStreamDescriptor`].
///
/// Calling neither [`window_pid`][Self::window_pid] nor
/// [`window_app`][Self::window_app] produces a monitor descriptor.
///
/// [builder-pattern]: https://doc.rust-lang.org/1.0.0/style/ownership/builders.html
pub struct AudioStreamBuilder {
    descriptor: AudioStreamDescriptor,
}

impl AudioStreamBuilder {
    /// Create a new descriptor for the video stream with `mapping_id`.
    pub fn new(mapping_id: impl Into<String>) -> Self {
        Self {
            descriptor: AudioStreamDescriptor {
                mapping_id: mapping_id.into(),
                identifier: None,
            },
        }
    }

    /// Identify the selected window by its positive host-namespace PID.
    #[must_use]
    pub fn window_pid(mut self, pid: u32) -> Self {
        self.descriptor.identifier = Some(AudioStreamIdentifier {
            type_: AudioStreamIdentifierType::Pid,
            pid: Some(pid),
            app_id: None,
            instance_id: None,
        });
        self
    }

    /// Identify the selected window by portal app ID and optional exact
    /// instance ID.
    #[must_use]
    pub fn window_app(
        mut self,
        app_id: impl Into<String>,
        instance_id: impl Into<Option<String>>,
    ) -> Self {
        self.descriptor.identifier = Some(AudioStreamIdentifier {
            type_: AudioStreamIdentifierType::App,
            pid: None,
            app_id: Some(app_id.into()),
            instance_id: instance_id.into(),
        });
        self
    }

    /// Build the [`AudioStreamDescriptor`].
    pub fn build(self) -> AudioStreamDescriptor {
        self.descriptor
    }
}

/// The implementation-interface result of a [`ScreencastImpl::start_cast`]
/// request.
#[derive(Default, Serialize, Deserialize, Type, Debug)]
#[zvariant(signature = "dict")]
pub struct Streams {
    #[serde(default, with = "as_value", skip_serializing_if = "Vec::is_empty")]
    streams: Vec<Stream>,
    #[serde(default, with = "as_value", skip_serializing_if = "Vec::is_empty")]
    audio_streams: Vec<AudioStreamDescriptor>,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    persist_mode: Option<PersistMode>,
    #[serde(default, with = "optional", skip_serializing_if = "Option::is_none")]
    restore_data: Option<(String, u32, OwnedValue)>,
}

impl Streams {
    /// The concrete video PipeWire streams.
    pub fn streams(&self) -> &[Stream] {
        &self.streams
    }

    /// The logical audio descriptors of approved sources.
    pub fn audio_streams(&self) -> &[AudioStreamDescriptor] {
        &self.audio_streams
    }

    /// The session's persist mode.
    pub fn persist_mode(&self) -> Option<PersistMode> {
        self.persist_mode
    }

    /// The session restore data.
    pub fn restore_data(&self) -> Option<&(String, u32, OwnedValue)> {
        self.restore_data.as_ref()
    }
}

/// A [builder-pattern] type to construct a backend [`Streams`] result.
///
/// [builder-pattern]: https://doc.rust-lang.org/1.0.0/style/ownership/builders.html
pub struct StreamsBuilder {
    streams: Streams,
}

impl StreamsBuilder {
    /// Create a new instance from the concrete video streams.
    pub fn new(streams: Vec<Stream>) -> Self {
        Self {
            streams: Streams {
                streams,
                audio_streams: Vec::new(),
                persist_mode: None,
                restore_data: None,
            },
        }
    }

    /// Set the logical audio descriptors.
    #[must_use]
    pub fn audio_streams(mut self, audio_streams: Vec<AudioStreamDescriptor>) -> Self {
        self.streams.audio_streams = audio_streams;
        self
    }

    /// Set the streams' persist mode.
    #[must_use]
    pub fn persist_mode(mut self, data: Option<impl Into<PersistMode>>) -> Self {
        self.streams.persist_mode = data.map(|m| m.into());
        self
    }

    /// Set the streams' optional restore data.
    #[must_use]
    pub fn restore_data(mut self, data: Option<(String, u32, impl Into<OwnedValue>)>) -> Self {
        self.streams.restore_data = data.map(|(s, u, d)| (s, u, d.into()));
        self
    }

    /// Build the [`Streams`].
    pub fn build(self) -> Streams {
        self.streams
    }
}

#[async_trait]
pub trait ScreencastImpl: RequestImpl + SessionImpl {
    #[doc(alias = "AvailableSourceTypes")]
    fn available_source_types(&self) -> BitFlags<SourceType>;

    #[doc(alias = "AvailableCursorModes")]
    fn available_cursor_mode(&self) -> BitFlags<CursorMode>;

    #[doc(alias = "CreateSession")]
    async fn create_session(
        &self,
        token: HandleToken,
        session_token: HandleToken,
        app_id: Option<MaybeAppID>,
        options: CreateSessionOptions,
    ) -> Result<CreateSessionResponse>;

    #[doc(alias = "SelectSources")]
    async fn select_sources(
        &self,
        token: HandleToken,
        session_token: HandleToken,
        app_id: Option<MaybeAppID>,
        options: SelectSourcesOptions,
    ) -> Result<SelectSourcesResponse>;

    #[doc(alias = "Start")]
    async fn start_cast(
        &self,
        token: HandleToken,
        session_token: HandleToken,
        app_id: Option<MaybeAppID>,
        window_identifier: Option<WindowIdentifierType>,
        options: StartCastOptions,
    ) -> Result<Streams>;
}

pub(crate) struct ScreencastInterface {
    imp: Arc<dyn ScreencastImpl>,
    spawn: Arc<dyn futures_util::task::Spawn + Send + Sync>,
    cnx: zbus::Connection,
    sessions: Arc<Mutex<SessionManager>>,
}

impl ScreencastInterface {
    pub fn new(
        imp: Arc<dyn ScreencastImpl>,
        cnx: zbus::Connection,
        spawn: Arc<dyn futures_util::task::Spawn + Send + Sync>,
        sessions: Arc<Mutex<SessionManager>>,
    ) -> Self {
        Self {
            imp,
            cnx,
            spawn,
            sessions,
        }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreencastInterface {
    #[zbus(
        property(emits_changed_signal = "const"),
        name = "AvailableSourceTypes"
    )]
    fn available_source_types(&self) -> u32 {
        let imp = Arc::clone(&self.imp);
        imp.available_source_types().bits()
    }

    #[zbus(
        property(emits_changed_signal = "const"),
        name = "AvailableCursorModes"
    )]
    fn available_cursor_mode(&self) -> u32 {
        let imp = Arc::clone(&self.imp);
        imp.available_cursor_mode().bits()
    }

    #[zbus(property(emits_changed_signal = "const"), name = "version")]
    fn version(&self) -> u32 {
        7
    }

    #[zbus(name = "CreateSession")]
    #[zbus(out_args("response", "results"))]
    async fn create_session(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: Optional<MaybeAppID>,
        options: CreateSessionOptions,
    ) -> Result<Response<CreateSessionResponse>> {
        let session_token = HandleToken::try_from(&session_handle).unwrap();
        {
            let sessions = self.sessions.lock().unwrap();
            if sessions.contains(&session_token) {
                let errormsg = format!("A session with handle `{session_token}` already exists");
                #[cfg(feature = "tracing")]
                tracing::error!("ScreencastInterface::create_session: {}", errormsg);
                return Err(PortalError::Exist(errormsg));
            }
        }

        let imp = Arc::clone(&self.imp);
        let token = session_token.clone();
        let result = Request::spawn(
            "ScreenCast::CreateSession",
            &self.cnx,
            handle.clone(),
            Arc::clone(&self.imp),
            Arc::clone(&self.spawn),
            async move {
                imp.create_session(
                    HandleToken::try_from(&handle).unwrap(),
                    token,
                    app_id.into(),
                    options,
                )
                .await
            },
        )
        .await;

        if result.is_ok() {
            #[cfg(feature = "tracing")]
            tracing::debug!(
                "ScreencastInterface::create_session: session with handle `{session_token}` created"
            );

            let session = Session::new(
                session_handle,
                Arc::clone(&self.sessions),
                Some(Arc::clone(&self.imp) as Arc<dyn SessionImpl>),
            );
            session.serve(self.cnx.clone()).await?;
            {
                let mut sessions = self.sessions.lock().unwrap();
                sessions.add(session);
            }
        } else {
            #[cfg(feature = "tracing")]
            tracing::error!(
                "ScreencastInterface::create_session: failed to create a session with handle `{session_token}`"
            );
        }

        result
    }

    #[zbus(name = "SelectSources")]
    #[zbus(out_args("response", "results"))]
    async fn select_sources(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: Optional<MaybeAppID>,
        options: SelectSourcesOptions,
    ) -> Result<Response<SelectSourcesResponse>> {
        let session_token = HandleToken::try_from(&session_handle).unwrap();
        {
            let sessions = self.sessions.lock().unwrap();
            sessions.try_contains(&session_token)?;
        }

        let imp = Arc::clone(&self.imp);
        Request::spawn(
            "ScreenCast::SelectSources",
            &self.cnx,
            handle.clone(),
            Arc::clone(&self.imp),
            Arc::clone(&self.spawn),
            async move {
                imp.select_sources(
                    HandleToken::try_from(&handle).unwrap(),
                    session_token,
                    app_id.into(),
                    options,
                )
                .await
            },
        )
        .await
    }

    #[zbus(name = "Start")]
    #[zbus(out_args("response", "results"))]
    async fn start(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: Optional<MaybeAppID>,
        window_identifier: Optional<WindowIdentifierType>,
        options: StartCastOptions,
    ) -> Result<Response<Streams>> {
        let session_token = HandleToken::try_from(&session_handle).unwrap();
        {
            let sessions = self.sessions.lock().unwrap();
            sessions.try_contains(&session_token)?;
        }

        let imp = Arc::clone(&self.imp);
        Request::spawn(
            "ScreenCast::Start",
            &self.cnx,
            handle.clone(),
            Arc::clone(&self.imp),
            Arc::clone(&self.spawn),
            async move {
                imp.start_cast(
                    HandleToken::try_from(&handle).unwrap(),
                    session_token,
                    app_id.into(),
                    window_identifier.into(),
                    options,
                )
                .await
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde::Deserialize;
    use zbus::zvariant::{Endian, Value, as_value::{self, optional}, serialized::Context, to_bytes};

    use super::*;

    #[derive(Deserialize, Type)]
    #[zvariant(signature = "dict")]
    struct WireDescriptor {
        #[serde(with = "as_value")]
        mapping_id: String,
        #[serde(default, with = "optional")]
        identifier: Option<WireIdentifier>,
    }

    #[derive(Deserialize, Type)]
    #[zvariant(signature = "dict")]
    struct WireIdentifier {
        #[serde(rename = "type", with = "as_value")]
        type_: String,
        #[serde(default, with = "optional")]
        pid: Option<u32>,
        #[serde(default, with = "optional")]
        app_id: Option<String>,
        #[serde(default, with = "optional")]
        instance_id: Option<String>,
    }

    #[test]
    fn implementation_select_sources_audio_deserialization() {
        let ctxt = Context::new_dbus(Endian::Little, 0);

        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("audio", Value::from(true));
        let encoded = to_bytes(ctxt, &options).unwrap();
        let decoded: SelectSourcesOptions = encoded.deserialize().unwrap().0;
        assert_eq!(decoded.is_audio(), Some(true));

        let options: HashMap<&str, Value<'_>> = HashMap::new();
        let encoded = to_bytes(ctxt, &options).unwrap();
        let decoded: SelectSourcesOptions = encoded.deserialize().unwrap().0;
        assert_eq!(decoded.is_audio(), None);
    }

    #[test]
    fn monitor_descriptor_serialization() {
        let ctxt = Context::new_dbus(Endian::Little, 0);
        let descriptor = AudioStreamBuilder::new("source-0").build();
        let encoded = to_bytes(ctxt, &descriptor).unwrap();
        let decoded: WireDescriptor = encoded.deserialize().unwrap().0;
        assert_eq!(decoded.mapping_id, "source-0");
        assert!(decoded.identifier.is_none());
    }

    #[test]
    fn pid_window_descriptor_serialization() {
        let ctxt = Context::new_dbus(Endian::Little, 0);
        let descriptor = AudioStreamBuilder::new("source-1").window_pid(1234).build();
        let encoded = to_bytes(ctxt, &descriptor).unwrap();
        let decoded: WireDescriptor = encoded.deserialize().unwrap().0;
        assert_eq!(decoded.mapping_id, "source-1");
        let identifier = decoded.identifier.unwrap();
        assert_eq!(identifier.type_, "pid");
        assert_eq!(identifier.pid, Some(1234));
        assert_eq!(identifier.app_id, None);
        assert_eq!(identifier.instance_id, None);
    }

    #[test]
    fn app_window_descriptor_serialization() {
        let ctxt = Context::new_dbus(Endian::Little, 0);

        let descriptor = AudioStreamBuilder::new("source-2")
            .window_app("org.example.App", Some("123456".to_string()))
            .build();
        let encoded = to_bytes(ctxt, &descriptor).unwrap();
        let decoded: WireDescriptor = encoded.deserialize().unwrap().0;
        let identifier = decoded.identifier.unwrap();
        assert_eq!(identifier.type_, "app");
        assert_eq!(identifier.app_id.as_deref(), Some("org.example.App"));
        assert_eq!(identifier.instance_id.as_deref(), Some("123456"));
        assert_eq!(identifier.pid, None);

        let descriptor = AudioStreamBuilder::new("source-3")
            .window_app("org.example.Snap", None)
            .build();
        let encoded = to_bytes(ctxt, &descriptor).unwrap();
        let decoded: WireDescriptor = encoded.deserialize().unwrap().0;
        let identifier = decoded.identifier.unwrap();
        assert_eq!(identifier.type_, "app");
        assert_eq!(identifier.app_id.as_deref(), Some("org.example.Snap"));
        assert_eq!(identifier.instance_id, None);
    }

    #[test]
    fn streams_result_serialization() {
        let ctxt = Context::new_dbus(Endian::Little, 0);
        let streams = StreamsBuilder::new(vec![
            StreamBuilder::new(42)
                .source_type(SourceType::Monitor)
                .mapping_id("source-0".to_string())
                .build(),
        ])
        .audio_streams(vec![AudioStreamBuilder::new("source-0").build()])
        .build();
        let encoded = to_bytes(ctxt, &streams).unwrap();
        let decoded: Streams = encoded.deserialize().unwrap().0;
        assert_eq!(decoded.streams().len(), 1);
        assert_eq!(decoded.streams()[0].pipe_wire_node_id(), 42);
        assert_eq!(decoded.audio_streams().len(), 1);
        assert_eq!(decoded.audio_streams()[0].mapping_id(), "source-0");
    }
}
