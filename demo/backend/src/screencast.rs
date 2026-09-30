use std::{collections::HashSet, sync::Mutex};

use ashpd::{
    MaybeAppID, WindowIdentifierType,
    backend::{
        Result,
        request::RequestImpl,
        screencast::{
            AudioStreamBuilder, ScreencastImpl, SelectSourcesOptions, SelectSourcesResponse,
            Streams, StreamsBuilder,
        },
        session::{CreateSessionResponse, SessionImpl},
    },
    desktop::{
        CreateSessionOptions, HandleToken,
        screencast::{CursorMode, SourceType, StartCastOptions, StreamBuilder},
    },
    enumflags2::BitFlags,
};
use async_trait::async_trait;

#[derive(Default)]
pub struct Screencast {
    audio_sessions: Mutex<HashSet<HandleToken>>,
}

#[async_trait]
impl RequestImpl for Screencast {
    async fn close(&self, token: HandleToken) {
        tracing::debug!("IN Close(): {token}");
    }
}

#[async_trait]
impl ScreencastImpl for Screencast {
    fn available_source_types(&self) -> BitFlags<SourceType> {
        SourceType::Monitor | SourceType::Window
    }

    fn available_cursor_mode(&self) -> BitFlags<CursorMode> {
        CursorMode::Hidden | CursorMode::Embedded | CursorMode::Metadata
    }

    async fn create_session(
        &self,
        _token: HandleToken,
        session_token: HandleToken,
        _app_id: Option<MaybeAppID>,
        _options: CreateSessionOptions,
    ) -> Result<CreateSessionResponse> {
        tracing::debug!("IN Screencast::create_session(): {session_token}");
        Ok(CreateSessionResponse::new(session_token))
    }

    async fn select_sources(
        &self,
        _token: HandleToken,
        session_token: HandleToken,
        _app_id: Option<MaybeAppID>,
        options: SelectSourcesOptions,
    ) -> Result<SelectSourcesResponse> {
        tracing::debug!("IN Screencast::select_sources(): {session_token}");
        if options.is_audio().unwrap_or(false) {
            self.audio_sessions.lock().unwrap().insert(session_token);
        }
        Ok(SelectSourcesResponse::default())
    }

    async fn start_cast(
        &self,
        _token: HandleToken,
        session_token: HandleToken,
        _app_id: Option<MaybeAppID>,
        _window_identifier: Option<WindowIdentifierType>,
        _options: StartCastOptions,
    ) -> Result<Streams> {
        tracing::debug!("IN Screencast::start_cast(): {session_token}");
        let mut builder = StreamsBuilder::new(vec![
            StreamBuilder::new(42)
                .source_type(SourceType::Monitor)
                .mapping_id("monitor-0".to_string())
                .build(),
        ]);
        if self.audio_sessions.lock().unwrap().remove(&session_token) {
            builder = builder.audio_streams(vec![AudioStreamBuilder::new("monitor-0").build()]);
        }
        Ok(builder.build())
    }
}

#[async_trait]
impl SessionImpl for Screencast {
    async fn session_closed(&self, session_token: HandleToken) -> Result<()> {
        tracing::debug!("IN Screencast::session_closed(): {session_token}");
        Ok(())
    }
}
