use thiserror::Error;

#[derive(Debug, Error)]
pub enum KafkaError {
    #[error("broker error: {0}")]
    Broker(String),
    #[error("missing mandatory header: {0}")]
    MissingHeader(&'static str),
    #[error("schema version mismatch: expected {expected}, got {got}")]
    SchemaMismatch { expected: String, got: String },
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("deserialization error: {0}")]
    Deserialization(String),
    #[error("tenant mismatch in blob ref")]
    TenantMismatch,
    #[error("invalid blob ref: {0}")]
    InvalidBlobRef(String),
    #[error("consumer lagged; messages dropped")]
    ConsumerLag,
    #[error("invalid uuid in header {header}: {source}")]
    InvalidHeaderUuid {
        header: &'static str,
        source: uuid::Error,
    },
    #[error("rdkafka error: {0}")]
    Rdkafka(#[from] rdkafka::error::KafkaError),
    #[error("max retries exceeded; message should be routed to DLQ")]
    MaxRetriesExceeded,
    #[error("malformed W3C traceparent header: {0}")]
    InvalidTraceparent(String),
}

impl KafkaError {
    /// Returns true if this error is terminal (message should go to DLQ, no retry).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            KafkaError::MissingHeader(_)
                | KafkaError::SchemaMismatch { .. }
                | KafkaError::Deserialization(_)
                | KafkaError::TenantMismatch
                | KafkaError::InvalidBlobRef(_)
                | KafkaError::InvalidHeaderUuid { .. }
                | KafkaError::InvalidTraceparent(_)
        )
    }

    /// Returns `true` when the error represents a local or protocol-level
    /// request timeout (`REQTMOUT` / `ApiVersionRequest failed: Local: Timed out`).
    ///
    /// Used by `rb-kafka-health` to detect the sustained reconnect-retry
    /// cascade that appears after a broker restart where the consumer
    /// cannot rejoin and produces a stream of consecutive timeout errors.
    #[must_use]
    pub fn is_reqtmout(&self) -> bool {
        use rdkafka::error::RDKafkaErrorCode;
        let Self::Rdkafka(inner) = self else {
            return false;
        };
        matches!(
            inner.rdkafka_error_code(),
            Some(
                RDKafkaErrorCode::TimedOut
                    | RDKafkaErrorCode::TimedOutQueue
                    | RDKafkaErrorCode::RequestTimedOut
                    | RDKafkaErrorCode::OperationTimedOut
            )
        )
    }

    /// Returns true when the error indicates the Kafka broker cluster is
    /// unreachable or not yet available (librdkafka lazy-connect scenarios).
    /// Callers should surface this as HTTP 503, not 500.
    #[must_use]
    pub fn is_broker_unavailable(&self) -> bool {
        use rdkafka::error::RDKafkaErrorCode;
        let KafkaError::Rdkafka(inner) = self else {
            return false;
        };
        matches!(
            inner.rdkafka_error_code(),
            Some(
                RDKafkaErrorCode::AllBrokersDown
                    | RDKafkaErrorCode::BrokerNotAvailable
                    | RDKafkaErrorCode::LeaderNotAvailable
                    | RDKafkaErrorCode::MessageTimedOut
                    | RDKafkaErrorCode::NetworkException
                    | RDKafkaErrorCode::OperationTimedOut
                    | RDKafkaErrorCode::RequestTimedOut
            )
        )
    }
}
