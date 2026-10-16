//! Shared building blocks for Peek: the framebuffer every backend draws
//! into, the input model, endpoints and the UI/backend session contract.

pub mod endpoint;
pub mod experience;
pub mod frame;
pub mod input;
pub mod session;

pub use endpoint::{Endpoint, ParseError, Protocol};
pub use experience::{ConnectionSpeed, Experience};
pub use frame::{Framebuffer, FrameWriter, Rect, SharedFrame, rgb, rgba};
pub use input::{InputEvent, KeyCode, MouseButton, char_to_keysym};
pub use session::{
    AudioChunk, AudioControl, AuthError, ConnectRequest, Credentials, CursorImage, CursorUpdate, EventSink, SessionCommand, SessionContext,
    SessionEvent, SessionHandle, SessionOptions, SessionStats, new_session,
};
