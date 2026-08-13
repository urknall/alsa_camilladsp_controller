//! Placeholder stdin PCM transport module.
//!
//! Will receive PCM from the ioplug ALSA plugin via a kernel pipe whose write
//! end is delivered over the Unix IPC socket. CamillaDSP is spawned with the
//! pipe read end as its stdin. No Rust code is in the PCM data path.
//! Currently unimplemented — ioplug backend is not yet active.
