//! AF_UNIX socket listener for the piCoreDSP IPC channel.
//!
//! Accepts connections from the ioplug ALSA plugin, deserialises
//! `PluginMessage` frames, and dispatches them to the controller core.
//! Will be implemented in Gate 6 (Milestone M6).
