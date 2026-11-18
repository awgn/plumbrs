use std::{collections::HashMap, error::Error};

use contatori::Monotone;
use hdrhistogram::Histogram;

#[derive(Debug)]
pub struct RealtimeStats {
    pub ok: Monotone,
    pub fail: Monotone,
    pub err: Monotone,
}

impl RealtimeStats {
    #[must_use]
    pub const fn new() -> Self {
        RealtimeStats {
            ok: Monotone::new().with_name("ok"),
            fail: Monotone::new().with_name("fail"),
            err: Monotone::new().with_name("err"),
        }
    }
}

impl Default for RealtimeStats {
    fn default() -> Self {
        Self::new()
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
        rt.ok.incr();
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
            rt.ok.incr();
            self.ok += 1;
            return;
        }

        rt.fail.incr();

        let num = code.as_u16();
        if let Some(val) = self.status.get_mut(&num) {
            *val += 1;
        } else {
            self.status.insert(num, 1);
        }
    }

    #[inline]
    pub fn set_error<E: Error + ?Sized>(&mut self, err: &E, rt: &RealtimeStats) {
        rt.err.incr();
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
    pub fn status_1xx(&self) -> u64 {
        self.status_class(100..200)
    }

    #[inline]
    pub fn status_2xx(&self) -> u64 {
        self.ok + self.status_class(200..300)
    }

    #[inline]
    pub fn status_3xx(&self) -> u64 {
        self.status_class(300..400)
    }

    #[inline]
    fn status_class(&self, range: std::ops::Range<u16>) -> u64 {
        self.status
            .iter()
            .filter(|(code, _)| range.contains(*code))
            .map(|(_, &count)| count)
            .sum()
    }

    #[inline]
    pub fn status_4xx(&self) -> u64 {
        self.status_class(400..500)
    }

    #[inline]
    pub fn status_5xx(&self) -> u64 {
        self.status_class(500..600)
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

#[cfg(test)]
mod tests {
    use super::*;
    use contatori::Observable;
    use std::sync::Arc;

    #[test]
    fn realtime_stats_tracks_ok_fail_and_errors_across_threads() {
        let rt = Arc::new(RealtimeStats::new());
        let mut handles = Vec::new();

        for _ in 0..4 {
            let rt = Arc::clone(&rt);
            handles.push(std::thread::spawn(move || {
                let mut stats = Statistics::new(false);
                stats.inc_ok(&rt);
                stats.set_http_status(hyper::StatusCode::OK, &rt);
                stats.set_http_status(hyper::StatusCode::NOT_FOUND, &rt);
                stats.set_http_status(hyper::StatusCode::INTERNAL_SERVER_ERROR, &rt);
                let io_err = std::io::Error::other("boom");
                stats.set_error(&io_err, &rt);
                stats
            }));
        }

        let total = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .fold(Statistics::default(), |acc, s| acc + s);

        assert_eq!(total.ok(), 8);
        assert_eq!(total.status_4xx(), 4);
        assert_eq!(total.status_5xx(), 4);
        assert_eq!(total.errors(), 4);

        assert_eq!(rt.ok.value().as_u64(), 8);
        assert_eq!(rt.fail.value().as_u64(), 8);
        assert_eq!(rt.err.value().as_u64(), 4);
    }
}
