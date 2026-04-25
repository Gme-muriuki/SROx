use crate::errors::pool_error::PoolError;
use std::{
    collections::VecDeque,
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, sync::Mutex};

#[derive(Debug)]
pub struct PooledConnection {
    pub stream: TcpStream,
    pub last_used: Instant,
}

pub struct ConnectionPool {
    addr: SocketAddr,
    max_size: usize,
    keep_alive_secs: Duration,
    idle: Mutex<VecDeque<PooledConnection>>,
}

impl ConnectionPool {
    pub fn new(addr: SocketAddr, max_size: usize, keep_alive_secs: u64) -> Self {
        Self {
            addr,
            max_size,
            keep_alive_secs: Duration::from_secs(keep_alive_secs),
            idle: Mutex::new(VecDeque::new()),
        }
    }

    /// Get a connection: reuse an idle one or open a new TcpStream
    pub async fn checkout(&self) -> Result<PooledConnection, PoolError> {
        let mut idle = self.idle.lock().await;
        let now = Instant::now();

        // Evict stale connection at the front.
        while let Some(conn) = idle.front() {
            if now.duration_since(conn.last_used) > self.keep_alive_secs {
                idle.pop_front();
            } else {
                break;
            }
        }

        if let Some(conn) = idle.pop_front() {
            // Reusing existing idle connection
            Ok(conn)
        } else {
            // No idle connection, open a new one.
            let stream = TcpStream::connect(self.addr).await?;
            Ok(PooledConnection {
                stream,
                last_used: Instant::now(),
            })
        }
    }

    pub async fn checkin(&self, mut conn: PooledConnection) {
        let mut idle = self.idle.lock().await;

        // If the pool is full, drop the connection
        if idle.len() >= self.max_size {
            return;
        }

        // Update last_used and push to back
        conn.last_used = Instant::now();
        idle.push_back(conn);
    }

    pub async fn evict_stale(&self) {
        let mut idle = self.idle.lock().await;
        let now = Instant::now();

        while let Some(conn) = idle.front() {
            if now.duration_since(conn.last_used) > self.keep_alive_secs {
                idle.pop_front();
            } else {
                break;
            }
        }
    }
}
