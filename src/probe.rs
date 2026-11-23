use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProbeKey {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineStatus {
    Unknown,
    Checking,
    Online { latency_ms: u64 },
    Offline,
}

pub fn probe_tcp(host: &str, port: u16, timeout: Duration) -> OnlineStatus {
    if let Ok(ip) = host.parse::<IpAddr>() {
        let addr = SocketAddr::new(ip, port);
        let start = Instant::now();
        return if TcpStream::connect_timeout(&addr, timeout).is_ok() {
            OnlineStatus::Online {
                latency_ms: start.elapsed().as_millis() as u64,
            }
        } else {
            OnlineStatus::Offline
        };
    }

    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return OnlineStatus::Offline;
    };

    let deadline = Instant::now() + timeout;
    for addr in addrs.take(2) {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        if remaining.is_zero() {
            break;
        }
        let start = Instant::now();
        if TcpStream::connect_timeout(&addr, remaining).is_ok() {
            return OnlineStatus::Online {
                latency_ms: start.elapsed().as_millis() as u64,
            };
        }
    }

    OnlineStatus::Offline
}

pub fn probe_batch(
    keys: Vec<ProbeKey>,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> HashMap<ProbeKey, OnlineStatus> {
    let mut seen = HashSet::new();
    let unique_keys: Vec<ProbeKey> = keys
        .into_iter()
        .filter(|k| seen.insert(k.clone()))
        .collect();

    if unique_keys.is_empty() || is_cancelled() {
        return HashMap::new();
    }

    let next_idx = AtomicUsize::new(0);
    let results = Mutex::new(HashMap::with_capacity(unique_keys.len()));
    let timeout = Duration::from_millis(1200);
    let worker_count = unique_keys.len().min(24);

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    if is_cancelled() {
                        break;
                    }
                    let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                    let Some(key) = unique_keys.get(idx) else {
                        break;
                    };
                    let status = probe_tcp(&key.host, key.port, timeout);
                    if let Ok(mut map) = results.lock() {
                        map.insert(key.clone(), status);
                    }
                }
            });
        }
    });

    results.into_inner().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn tcp_probe_and_batch_work() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let live_port = listener.local_addr().unwrap().port();

        let closed_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = closed_listener.local_addr().unwrap().port();
        drop(closed_listener);

        let live_key = ProbeKey {
            host: "127.0.0.1".into(),
            port: live_port,
        };
        let dead_key = ProbeKey {
            host: "127.0.0.1".into(),
            port: dead_port,
        };

        assert!(matches!(
            probe_tcp("127.0.0.1", live_port, Duration::from_millis(400)),
            OnlineStatus::Online { .. }
        ));
        assert_eq!(
            probe_tcp("127.0.0.1", dead_port, Duration::from_millis(300)),
            OnlineStatus::Offline
        );

        let batch = probe_batch(
            vec![live_key.clone(), live_key.clone(), dead_key.clone()],
            &|| false,
        );
        assert_eq!(batch.len(), 2);
        assert!(matches!(
            batch.get(&live_key),
            Some(OnlineStatus::Online { .. })
        ));
        assert_eq!(batch.get(&dead_key), Some(&OnlineStatus::Offline));

        let cancelled = probe_batch(vec![dead_key], &|| true);
        assert!(cancelled.is_empty());
    }
}
