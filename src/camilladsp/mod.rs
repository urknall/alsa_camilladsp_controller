//! CamillaDSP interaction modules: WebSocket client, ALSA capture listener,
//! process supervisor, and stdin PCM transport.
pub mod alsa_capture;
pub mod stdin_capture;
pub mod supervisor;
pub mod websocket;
