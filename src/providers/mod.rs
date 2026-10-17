//! One module per streaming service. Each exposes `resolve` (link -> metadata)
//! and `search` (metadata -> candidates on that service).

pub mod spotify;
pub mod youtube;

pub const USER_AGENT: &str = concat!(
    "tunebridge/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/OptimoSupreme/tunebridge)"
);
