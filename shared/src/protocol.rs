use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageType {
    TlsHandshake,
    HttpRequest,
    HttpResponse,
    FileDownloadRequest,
    FileDownloadChunk,
    FileUploadRequest,
    FileUploadChunk,
    GamePacket,
    ConnectionClose,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub message_type: MessageType,
    pub payload: Vec<u8>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub sequence: u64,
}

impl Message {
    pub fn new(message_type: MessageType, payload: Vec<u8>, session_id: Uuid) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            message_type,
            payload,
            timestamp: chrono::Utc::now(),
            sequence: 0,
        }
    }

    pub fn with_sequence(mut self, sequence: u64) -> Self {
        self.sequence = sequence;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConnectionType {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UserActivity {
    WebBrowsing {
        pages_to_visit: u32,
        requests_per_page: (u32, u32),
    },
    FileDownload {
        file_size: u64,
        chunk_size: (u32, u32),
    },
    FileUpload {
        file_size: u64,
        chunk_size: (u32, u32),
    },
    Gaming {
        packets_per_second: u32,
        packet_size_range: (u32, u32),
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorType {
    ConnectionFailed,
    NetworkTimeout,
    SerializationError,
    MessageTooLarge,
    UnexpectedDisconnection,
    ProtocolError,
    IoError,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub error_type: ErrorType,
    pub message: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub connection_id: Uuid,
    pub context: Option<String>,
}

impl ErrorDetail {
    pub fn new(
        error_type: ErrorType,
        message: String,
        connection_id: Uuid,
        context: Option<String>,
    ) -> Self {
        Self {
            error_type,
            message,
            timestamp: chrono::Utc::now(),
            connection_id,
            context,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub id: Uuid,
    pub connection_type: ConnectionType,
    pub local_addr: SocketAddr,
    pub remote_addr: SocketAddr,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub packets_sent: u64,
    pub packets_received: u64,
    pub activity: UserActivity,
}

impl ConnectionInfo {
    pub fn new(
        connection_type: ConnectionType,
        local_addr: SocketAddr,
        remote_addr: SocketAddr,
        activity: UserActivity,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            connection_type,
            local_addr,
            remote_addr,
            created_at: chrono::Utc::now(),
            bytes_sent: 0,
            bytes_received: 0,
            packets_sent: 0,
            packets_received: 0,
            activity,
        }
    }
}