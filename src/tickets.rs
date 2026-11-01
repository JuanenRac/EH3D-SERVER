//! Tickets: a way to open the camera or the websocket from a place that cannot send a header (a browser's `<img>` and `WebSocket`) without putting the
//! token of the session in the address, where it would stay in the history, in the logs of a proxy and in the `Referer`.
//!
//! A client that is logged in asks for a ticket (`POST /api/v1/tickets`); the ticket is good for one use within half a minute, and the address carries
//! only the ticket. The server keeps which session it was made for, so the request is judged as that session.

use std::collections::HashMap;

/// How long a ticket can wait to be used, in seconds.
pub const TICKET_SECONDS: u64 = 30;
/// The most tickets that wait at once.
const MAX_PENDING: usize = 512;

#[derive(Default)]
pub struct Tickets {
    /// ticket -> (the token of the session it was made for, when it expires)
    pending: HashMap<String, (String, u64)>,
}

impl Tickets {
    /// A new ticket for the session of `token`; `None` when there are too many waiting.
    pub fn issue(&mut self, token: &str, now: u64) -> Option<String> {
        self.pending.retain(|_, (_, until)| *until > now);
        if self.pending.len() >= MAX_PENDING {
            return None;
        }
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        let ticket = format!("t_{}", hex::encode(bytes));
        self.pending
            .insert(ticket.clone(), (token.to_string(), now + TICKET_SECONDS));
        Some(ticket)
    }

    /// The token of the session a ticket was made for. The ticket is used up, whether or not it was still good.
    pub fn redeem(&mut self, ticket: &str, now: u64) -> Option<String> {
        let (token, until) = self.pending.remove(ticket)?;
        (until > now).then_some(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_is_good_once_and_for_a_short_time() {
        let mut t = Tickets::default();
        let a = t.issue("session-token", 1000).unwrap();
        assert!(a.starts_with("t_") && !a.contains("session-token"));
        assert_eq!(t.redeem(&a, 1010).as_deref(), Some("session-token"));
        assert_eq!(t.redeem(&a, 1011), None, "used up");
        let b = t.issue("s", 1000).unwrap();
        assert_eq!(t.redeem(&b, 1000 + TICKET_SECONDS), None, "expired");
        assert_eq!(t.redeem("t_unknown", 1000), None);
    }

    #[test]
    fn too_many_waiting_tickets_are_refused_until_some_expire() {
        let mut t = Tickets::default();
        for _ in 0..MAX_PENDING {
            assert!(t.issue("s", 1000).is_some());
        }
        assert!(t.issue("s", 1000).is_none());
        assert!(t.issue("s", 1000 + TICKET_SECONDS).is_some());
    }
}
