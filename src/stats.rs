use std::{collections::HashMap, error::Error, sync::atomic::AtomicU64};

use hdrhistogram::Histogram;

#[repr(C)]
#[repr(align(64))]
pub struct RealtimeStats {
    pub ok: AtomicU64,
    pub fail: AtomicU64,
    pub err: AtomicU64,
}

impl Default for RealtimeStats {
    fn default() -> Self {
        RealtimeStats {
            ok: AtomicU64::new(0),
            fail: AtomicU64::new(0),
            err: AtomicU64::new(0),
        }
    }
}

#[derive(Debug, PartialEq, Default, Clone)]
pub struct Statistics {
    ok: u64,
    conn: u64,
    status: HashMap<u16, u64>,
    err: HashMap<String, u64>,
    idle: f64,
    pub latency: Option<Histogram<u64>>,
}

impl Statistics {
    pub fn new(with_latency: bool) -> Self {
        Statistics {
            ok: 0,
            conn: 0,
            status: HashMap::new(),
            err: HashMap::new(),
            idle: 0.0,
            latency: if with_latency {
                Some(
                    Histogram::<u64>::new_with_bounds(1, 10000000, 3)
                        .expect("failed to create histogram"),
                )
            } else {
                None
            },
        }
    }

    #[inline]
    pub fn inc_ok(&mut self, rt: &RealtimeStats) {
        rt.ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.ok += 1;
    }

    #[inline]
    pub fn inc_conn(&mut self) {
        self.conn += 1;
    }

    #[inline]
    pub fn idle_time(&mut self, idle: f64) {
        self.idle = idle;
    }

    #[inline]
    pub fn http_status(&self) -> &HashMap<u16, u64> {
        &self.status
    }

    #[inline]
    pub fn errors_map(&self) -> &HashMap<String, u64> {
        &self.err
    }

    #[inline]
    pub fn set_http_status(&mut self, code: hyper::StatusCode, rt: &RealtimeStats) {
        if matches!(code, hyper::StatusCode::OK) {
            rt.ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.ok += 1;
            return;
        }

        rt.fail.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let num = code.as_u16();
        if let Some(val) = self.status.get_mut(&num) {
            *val += 1;
        } else {
            self.status.insert(num, 1);
        }
    }

    #[inline]
    pub fn set_error<E: Error + ?Sized>(&mut self, err: &E, rt: &RealtimeStats) {
        rt.err.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let err_kind = err.to_string();
        if let Some(val) = self.err.get_mut(&err_kind) {
            *val += 1;
        } else {
            self.err.insert(err_kind, 1);
        }
    }

    #[inline]
    pub fn ok(&self) -> u64 {
        self.ok
    }

    #[inline]
    pub fn conn(&self) -> u64 {
        self.conn
    }

    #[inline]
    pub fn idle(&self) -> f64 {
        self.idle
    }

    #[inline]
    pub fn errors(&self) -> u64 {
        self.err.values().sum()
    }

    #[inline]
    pub fn status_3xx(&self) -> u64 {
        self.status
            .iter()
            .filter(|(code, _)| (300..400).contains(*code))
            .map(|(_, &count)| count)
            .sum()
    }

    #[inline]
    pub fn status_4xx(&self) -> u64 {
        self.status
            .iter()
            .filter(|(code, _)| (400..500).contains(*code))
            .map(|(_, &count)| count)
            .sum()
    }

    #[inline]
    pub fn status_5xx(&self) -> u64 {
        self.status
            .iter()
            .filter(|(code, _)| (500..600).contains(*code))
            .map(|(_, &count)| count)
            .sum()
    }
}

impl std::ops::Add for Statistics {
    type Output = Statistics;

    fn add(self, other: Statistics) -> Statistics {
        let mut hs = self.status;
        for (key, value) in other.status {
            if let Some(acc_value) = hs.get_mut(&key) {
                *acc_value += value;
            } else {
                hs.insert(key, value);
            }
        }

        let mut e = self.err;
        for (key, value) in other.err {
            if let Some(acc_value) = e.get_mut(&key) {
                *acc_value += value;
            } else {
                e.insert(key, value);
            }
        }

        Statistics {
            ok: self.ok + other.ok,
            conn: self.conn + other.conn,
            err: e,
            status: hs,
            idle: self.idle + other.idle,
            latency: match (self.latency, other.latency) {
                (Some(h1), Some(h2)) => Some(h1.add(&h2)),
                (Some(h), None) | (None, Some(h)) => Some(h),
                (None, None) => None,
            },
        }
    }
}
