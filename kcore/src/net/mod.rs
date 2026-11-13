//! Network protocols without I/O: packet formats and TCP. The kernel's stack (kernel/src/net)
//! moves packets between these and the network card and sockets.
pub mod tcp;
pub mod wire;
