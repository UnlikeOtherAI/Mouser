use std::net::SocketAddr;

use crate::NetError;

pub(crate) struct DialErrors {
    attempts: Vec<DialAttempt>,
}

struct DialAttempt {
    addr: SocketAddr,
    error: NetError,
    timed_out: bool,
}

impl DialErrors {
    pub(crate) fn new() -> Self {
        Self {
            attempts: Vec::new(),
        }
    }

    pub(crate) fn push_error(&mut self, addr: SocketAddr, error: NetError) {
        self.attempts.push(DialAttempt {
            addr,
            error,
            timed_out: false,
        });
    }

    pub(crate) fn push_timeout(&mut self, addr: SocketAddr) {
        self.attempts.push(DialAttempt {
            addr,
            error: NetError::Connect(format!("timed out dialing {addr}")),
            timed_out: true,
        });
    }

    pub(crate) fn finish(self) -> NetError {
        if self.attempts.len() == 1 {
            if let Some(attempt) = self.attempts.into_iter().next() {
                return attempt.error;
            }
            return no_dialable_address();
        }

        let Some(primary) = self
            .attempts
            .iter()
            .find(|attempt| !attempt.timed_out)
            .or_else(|| self.attempts.last())
        else {
            return no_dialable_address();
        };

        let details = self
            .attempts
            .iter()
            .map(|attempt| format!("{}: {}", attempt.addr, attempt.error))
            .collect::<Vec<_>>()
            .join("; ");
        let message = format!(
            "{}; candidate dial errors: {details}",
            error_detail(&primary.error)
        );
        with_detail_like(&primary.error, message)
    }
}

fn no_dialable_address() -> NetError {
    NetError::Connect("no dialable address for peer".to_string())
}

fn error_detail(error: &NetError) -> &str {
    match error {
        NetError::Identity(detail)
        | NetError::Tls(detail)
        | NetError::Io(detail)
        | NetError::Connect(detail)
        | NetError::Frame(detail)
        | NetError::Datagram(detail)
        | NetError::Discovery(detail) => detail,
    }
}

fn with_detail_like(error: &NetError, detail: String) -> NetError {
    match error {
        NetError::Identity(_) => NetError::Identity(detail),
        NetError::Tls(_) => NetError::Tls(detail),
        NetError::Io(_) => NetError::Io(detail),
        NetError::Connect(_) => NetError::Connect(detail),
        NetError::Frame(_) => NetError::Frame(detail),
        NetError::Datagram(_) => NetError::Datagram(detail),
        NetError::Discovery(_) => NetError::Discovery(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_prefers_informative_error_over_trailing_timeout() {
        let first: SocketAddr = "127.0.0.1:61600".parse().expect("first addr");
        let second: SocketAddr = "127.0.0.1:61601".parse().expect("second addr");
        let mut errors = DialErrors::new();
        errors.push_error(
            first,
            NetError::Connect("certificate pin mismatch".to_string()),
        );
        errors.push_timeout(second);

        let text = errors.finish().to_string();

        assert!(
            text.starts_with("connect: certificate pin mismatch"),
            "informative error should lead, got {text}"
        );
        assert!(
            text.contains("timed out dialing 127.0.0.1:61601"),
            "combined error should keep the timeout detail, got {text}"
        );
    }
}
