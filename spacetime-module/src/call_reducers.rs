use spacetimedb::{Identity, ReducerContext, SpacetimeType, Table};

#[derive(SpacetimeType, Clone, Debug, PartialEq)]
pub enum CallState {
    Ringing,
    Active,
    Ended,
}

#[spacetimedb::table(accessor = call_session, public)]
pub struct CallSession {
    #[primary_key]
    #[auto_inc]
    pub session_id: u64,
    pub caller: Identity,
    pub callee: Identity,
    pub state: CallState,
}

#[spacetimedb::table(accessor = video_frame, public, event)]
pub struct VideoFrame {
    pub session_id: u64,
    pub from: Identity,
    pub seq: u32,
    pub codec: u8,
    pub is_keyframe: bool,
    pub data: Vec<u8>,
}

#[spacetimedb::table(accessor = audio_frame, public, event)]
pub struct AudioFrame {
    pub session_id: u64,
    pub from: Identity,
    pub seq: u32,
    pub pcm: Vec<u8>,
}


#[spacetimedb::reducer]
pub fn request_call(ctx: &ReducerContext, callee: Identity) -> Result<(), String> {
    ctx.db.call_session().insert(CallSession {
        session_id: 0,
        caller: ctx.sender(),
        callee,
        state: CallState::Ringing,
    });
    Ok(())
}

 #[spacetimedb::reducer]
  pub fn accept_call(
      ctx: &ReducerContext,
      session_id: u64,
  ) -> Result<(), String> {
    let session = ctx.db.call_session().session_id().find(session_id).ok_or("Session Not found")?;

    if session.callee != ctx.sender() {
        return Err("Not the callee".into());
    }

    ctx.db.call_session().session_id().update(CallSession {
        state: CallState::Active,
        ..session
    });
    Ok(())
  }


  #[spacetimedb::reducer]
  pub fn end_call(
      ctx: &ReducerContext,
      session_id: u64,
  ) -> Result<(), String> {
    let session = ctx.db.call_session().session_id().find(session_id).ok_or("Session not found")?;
    ctx.db.call_session().session_id().update(CallSession {
        state: CallState::Ended,
        ..session
    });
    Ok(())
  }

#[spacetimedb::reducer]
pub fn send_video_frame(
    ctx: &ReducerContext,
    session_id: u64,
    seq: u32,
    codec: u8,
    is_keyframe: bool,
    data: Vec<u8>,
) {
    ctx.db.video_frame().insert(VideoFrame {
        session_id,
        from: ctx.sender(),
        seq,
        codec,
        is_keyframe,
        data,
    });
}


  #[spacetimedb::reducer]
  pub fn send_audio_frame(
      ctx: &ReducerContext,
      session_id: u64,
      seq: u32,
      pcm: Vec<u8>,
  ) {

    ctx.db.audio_frame().insert(AudioFrame {
        session_id,
        from: ctx.sender(),
        seq,
        pcm,
    });
  }
