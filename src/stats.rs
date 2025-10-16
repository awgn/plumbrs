use std::collections::HashMap;

#[derive(Debug, PartialEq, Default, Clone)]
pub struct Statistics {
    pub ok: u64,
    pub err: HashMap<String, u64>,
    pub http_status: HashMap<u16, u64>,
    pub idle: f64,
}

impl Statistics {
    #[inline]
    pub fn http_status(&mut self, code: hyper::StatusCode) {
        let num = code.as_u16();
        if let Some(val) = self.http_status.get_mut(&num) {
            *val += 1;
        } else {
            self.http_status.insert(num, 1);
        }
    }

    #[inline]
    pub fn err(&mut self, kind: String) {
        if let Some(val) = self.err.get_mut(&kind) {
            *val += 1;
        } else {
            self.err.insert(kind, 1);
        }
    }

    #[inline]
    pub fn total_errors(&self) -> u64 {
        self.err.values().sum()
    }

    #[inline]
    pub fn total_status_3xx(&self) -> u64 {
        self.http_status
            .iter()
            .filter(|(code, _)| (300..400).contains(*code))
            .map(|(_, &count)| count)
            .sum()
    }

    #[inline]
    pub fn total_status_4xx(&self) -> u64 {
        self.http_status
            .iter()
            .filter(|(code, _)| (400..500).contains(*code))
            .map(|(_, &count)| count)
            .sum()
    }

    #[inline]
    pub fn total_status_5xx(&self) -> u64 {
        self.http_status
            .iter()
            .filter(|(code, _)| (500..600).contains(*code))
            .map(|(_, &count)| count)
            .sum()
    }
}

impl std::ops::Add for Statistics {
    type Output = Statistics;

    fn add(self, other: Statistics) -> Statistics {
        let mut hs = self.http_status;
        for (key, value) in other.http_status {
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
            err: e,
            http_status: hs,
            idle: self.idle + other.idle,
        }
    }
}
