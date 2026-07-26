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
    pub agent_id: u64,
    pub caller: Identity,
    pub callee: Identity,
    pub state: CallState,
}

#[spacetimedb::table(accessor = video_frame, public, event)]
pub struct VideoFrame {
    pub agent_id: u64,
    pub from: Identity,
    pub seq: u32,
    pub codec: u8,
    pub is_keyframe: bool,
    pub data: Vec<u8>,
}

#[spacetimedb::table(accessor = audio_frame, public, event)]
pub struct AudioFrame {
    pub agent_id: u64,
    pub from: Identity,
    pub seq: u32,
    pub pcm: Vec<u8>,
}


#[spacetimedb::reducer]
pub fn request_call(ctx: &ReducerContext, callee: Identity) -> Result<(), String> {
    ctx.db.call_session().insert(CallSession {
        agent_id: 0,
        caller: ctx.sender(),
        callee,
        state: CallState::Ringing,
    });
    Ok(())
}

 #[spacetimedb::reducer]
  pub fn accept_call(
      ctx: &ReducerContext,
      agent_id: u64,
  ) -> Result<(), String> {
    let agent = ctx.db.call_session().agent_id().find(agent_id).ok_or("Agent Not found")?;

    if agent.callee != ctx.sender() {
        return Err("Not the callee".into());
    }

    ctx.db.call_session().agent_id().update(CallSession {
        state: CallState::Active,
        ..agent
    });
    Ok(())
  }


  #[spacetimedb::reducer]
  pub fn end_call(
      ctx: &ReducerContext,
      agent_id: u64,
  ) -> Result<(), String> {
    let agent = ctx.db.call_session().agent_id().find(agent_id).ok_or("Agent not found")?;
    ctx.db.call_session().agent_id().update(CallSession {
        state: CallState::Ended,
        ..agent
    });
    Ok(())
  }

#[spacetimedb::reducer]
pub fn send_video_frame(
    ctx: &ReducerContext,
    agent_id: u64,
    seq: u32,
    codec: u8,
    is_keyframe: bool,
    data: Vec<u8>,
) {
    ctx.db.video_frame().insert(VideoFrame {
        agent_id,
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
      agent_id: u64,
      seq: u32,
      pcm: Vec<u8>,
  ) {

    ctx.db.audio_frame().insert(AudioFrame {
        agent_id,
        from: ctx.sender(),
        seq,
        pcm,
    });
  }
