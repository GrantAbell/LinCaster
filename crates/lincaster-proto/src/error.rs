use thiserror::Error;

#[derive(Debug, Error)]
pub enum RodeError {
    #[error("Failed to load config from '{0}': {1}")]
    ConfigLoad(String, String),

    #[error("Config parse error: {0}")]
    ConfigParse(String),

    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    #[error("Device error: {0}")]
    DeviceError(String),

    #[error("PipeWire error: {0}")]
    PipeWire(String),

    #[error("Graph error: {0}")]
    Graph(String),

    #[error("Bus not found: {0}")]
    BusNotFound(String),

    #[error("Invalid gain value: {0} (must be 0.0..=1.0)")]
    InvalidGain(f32),

    #[error("State persistence error: {0}")]
    StatePersist(String),

    #[error("DBus error: {0}")]
    DBus(String),
}
