//! Protections against floods and address exhaustion, after Bullnym's
//! (`src/rate_limit.rs`, `src/ip_whitelist.rs`,
//! `docs/architecture/abuse-and-readiness.md`).
//!
//! - Callers: IPv6 callers are grouped by /56, so a routed /64 cannot pose
//!   as 2^64 sources. `X-Forwarded-For` is read only when the connection
//!   comes from a configured proxy.
//! - Limits per surface: hot per-source counters in memory, swept
//!   periodically; shared counters (distinct names per source, new
//!   identities per source, callbacks per nym) in Postgres.
//! - Fail closed per payment option: each backend has a health state from
//!   periodic probes. A payment option whose backend is unhealthy is not
//!   offered, with one fixed 503 message.
//! - Infrastructure bypass, for tests only: an IP allowlist, and a
//!   certification token from allowed sources. They skip the rate limits,
//!   never the caps or the health checks.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use tokio_postgres::GenericClient;

use crate::App;
use crate::error::{ApiError, ApiResult};


/// The header that carries the certification token.
pub const CERTIFICATION_HEADER: &str = "x-bullnym-certification-token";

/// The rate-limit key of a caller: the address for IPv4, the /56 for IPv6.
pub fn source_key(ip: IpAddr) -> String {
	match ip {
		IpAddr::V4(v4) => format!("v4:{}", v4),
		IpAddr::V6(v6) => {
			let o = v6.octets();
			format!(
				"v6:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}00::/56",
				o[0], o[1], o[2], o[3], o[4], o[5], o[6],
			)
		},
	}
}

/// A list of addresses and networks.
#[derive(Debug, Clone, Default)]
pub struct IpList {
	nets: Vec<IpNet>,
}

impl IpList {
	/// Parses addresses and CIDR blocks. A malformed entry fails the whole
	/// list, so a typo cannot silently empty it.
	pub fn parse(entries: &[String]) -> anyhow::Result<IpList> {
		let mut nets = Vec::with_capacity(entries.len());
		for entry in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
			let net = if entry.contains('/') {
				IpNet::from_str(entry).map_err(|e| anyhow::anyhow!("invalid network {:?}: {}", entry, e))?
			} else {
				IpNet::from(IpAddr::from_str(entry).map_err(|e| anyhow::anyhow!("invalid address {:?}: {}", entry, e))?)
			};
			nets.push(net);
		}
		Ok(IpList { nets })
	}

	pub fn contains(&self, ip: IpAddr) -> bool {
		self.nets.iter().any(|n| n.contains(&ip))
	}
}

/// The caller's address. When the connection comes from one of
/// `trusted_proxies`, it is the rightmost `X-Forwarded-For` entry that is
/// not itself a trusted proxy: the proxy appends the address it saw, and
/// anything to its left came from the client. Otherwise the header is
/// ignored.
pub fn caller_ip(peer: IpAddr, forwarded_for: Option<&str>, trusted_proxies: &IpList) -> IpAddr {
	if !trusted_proxies.contains(peer) {
		return peer;
	}
	let Some(xff) = forwarded_for else { return peer };
	for entry in xff.rsplit(',').map(str::trim) {
		match IpAddr::from_str(entry) {
			Ok(ip) if trusted_proxies.contains(ip) => continue,
			Ok(ip) => return ip,
			Err(_) => return peer,
		}
	}
	peer
}

/// Protection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
	/// Proxies whose `X-Forwarded-For` header is read. Empty: the header is
	/// never read.
	#[serde(default)]
	pub trusted_proxies: Vec<String>,
	/// Registrations (nyms and stores) per source per minute.
	#[serde(default = "d5")]
	pub register_per_minute: u32,
	/// New registering Ark addresses per source per hour.
	#[serde(default = "d10")]
	pub register_identities_per_hour: u32,
	/// The most nyms the service holds.
	#[serde(default = "d100000")]
	pub max_nyms: u64,
	/// Name lookups (nym lookup, LNURL metadata, checkout page data) per
	/// source per minute.
	#[serde(default = "d30")]
	pub lookups_per_minute: u32,
	/// Distinct names a source can look up per hour.
	#[serde(default = "d20")]
	pub lookup_names_per_hour: u32,
	/// LNURL callbacks per source per minute.
	#[serde(default = "d30")]
	pub callbacks_per_minute: u32,
	/// LNURL callbacks per nym per minute, from all sources.
	#[serde(default = "d60")]
	pub callbacks_per_nym_per_minute: u32,
	/// Sale creations per source per minute: loose enough for a busy
	/// counter on one address.
	#[serde(default = "d60")]
	pub sales_per_minute: u32,
	/// Sale status reads per source per minute, long-polls included.
	#[serde(default = "d600")]
	pub status_reads_per_minute: u32,
	/// Owner login requests per source per minute.
	#[serde(default = "d20")]
	pub owner_per_minute: u32,
	/// Open Lightning Address invoices per nym.
	#[serde(default = "d20")]
	pub max_open_invoices_per_nym: u32,
	/// Open invoices of all nyms and sales together. Each one is a hold
	/// invoice on the Ark server and a pending receive in barkd.
	#[serde(default = "d2000")]
	pub max_open_invoices: u32,
	/// Open, unpaid sales per store. A store whose sales get paid does not
	/// reach it.
	#[serde(default = "d50")]
	pub max_open_sales_per_store: u32,
	/// New invoices per second from public traffic, all callers together.
	/// The watchers have their own connections and are never throttled by
	/// it.
	#[serde(default = "d20")]
	pub public_invoices_per_second: u32,
	/// Callers that skip the rate limits. For tests only.
	#[serde(default)]
	pub ip_allowlist: Vec<String>,
}

fn d5() -> u32 { 5 }
fn d10() -> u32 { 10 }
fn d20() -> u32 { 20 }
fn d30() -> u32 { 30 }
fn d50() -> u32 { 50 }
fn d60() -> u32 { 60 }
fn d600() -> u32 { 600 }
fn d2000() -> u32 { 2000 }
fn d100000() -> u64 { 100_000 }

impl Default for Limits {
	fn default() -> Limits {
		// Every field has a serde default.
		serde_json::from_str("{}").expect("defaults")
	}
}

/// Certification: a token that, from an allowed source, skips the rate
/// limits. For deterministic tests of a deployment only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Certification {
	#[serde(default)]
	pub enabled: bool,
	#[serde(default, skip_serializing)]
	pub token: Option<String>,
	#[serde(default)]
	pub allowed_sources: Vec<String>,
}

/// Who is calling.
#[derive(Debug, Clone)]
pub struct Caller {
	pub ip: IpAddr,
	pub source: String,
	/// Allowlisted or certified: skips the rate limits.
	pub bypass: bool,
}

/// The per-surface buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
	Register,
	Lookup,
	Callback,
	SaleCreate,
	StatusRead,
	Owner,
}

impl Surface {
	fn name(&self) -> &'static str {
		match self {
			Surface::Register => "register",
			Surface::Lookup => "lookup",
			Surface::Callback => "callback",
			Surface::SaleCreate => "sale",
			Surface::StatusRead => "status",
			Surface::Owner => "owner",
		}
	}
}

/// A sliding-window counter per key, in memory. Checking and recording is
/// one step under the lock, so two callers cannot both pass the limit.
#[derive(Default)]
pub struct SlidingWindows {
	map: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl SlidingWindows {
	/// Records a hit for `key` unless it already has `limit` hits within
	/// `window`. A limit of 0 means no limit.
	pub fn check(&self, key: &str, limit: u32, window: Duration, now: Instant) -> bool {
		if limit == 0 {
			return true;
		}
		let mut map = self.map.lock().expect("poisoned");
		let hits = map.entry(key.to_string()).or_default();
		let cutoff = now.checked_sub(window).unwrap_or(now);
		while hits.front().is_some_and(|t| *t < cutoff) {
			hits.pop_front();
		}
		if hits.len() as u32 >= limit {
			return false;
		}
		hits.push_back(now);
		true
	}

	/// Drops keys without a hit in `max_age`, so one-off sources do not
	/// grow the map forever.
	pub fn sweep(&self, max_age: Duration, now: Instant) -> usize {
		let cutoff = now.checked_sub(max_age).unwrap_or(now);
		let mut map = self.map.lock().expect("poisoned");
		let before = map.len();
		map.retain(|_, hits| hits.back().is_some_and(|t| *t >= cutoff));
		before - map.len()
	}
}

/// A token bucket refilled every second.
pub struct TokenBucket {
	state: Mutex<(u32, Instant)>,
	rate: u32,
}

impl TokenBucket {
	pub fn new(rate_per_sec: u32) -> TokenBucket {
		let rate = rate_per_sec.max(1);
		TokenBucket { state: Mutex::new((rate, Instant::now())), rate }
	}

	pub fn try_take(&self, now: Instant) -> bool {
		let mut s = self.state.lock().expect("poisoned");
		let elapsed = now.saturating_duration_since(s.1);
		if elapsed >= Duration::from_secs(1) {
			let add = (elapsed.as_secs() as u32).saturating_mul(self.rate);
			s.0 = s.0.saturating_add(add).min(self.rate);
			s.1 = now;
		}
		if s.0 == 0 {
			return false;
		}
		s.0 -= 1;
		true
	}
}

/// The backends a payment option depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
	Barkd,
	ArkServer,
	Bitcoind,
}

/// The health of one backend, from consecutive probe results: it starts
/// closed, opens on its first success, closes after `CLOSE_AFTER` failures
/// in a row, and reopens after `REOPEN_AFTER` successes in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HealthState {
	pub open: bool,
	pub ever_opened: bool,
	pub failures: u32,
	pub successes: u32,
}

pub const CLOSE_AFTER: u32 = 3;
pub const REOPEN_AFTER: u32 = 2;

impl HealthState {
	pub fn record(&mut self, ok: bool) {
		if ok {
			self.successes = self.successes.saturating_add(1);
			self.failures = 0;
			if !self.open && (!self.ever_opened || self.successes >= REOPEN_AFTER) {
				self.open = true;
				self.ever_opened = true;
			}
		} else {
			self.failures = self.failures.saturating_add(1);
			self.successes = 0;
			if self.open && self.failures >= CLOSE_AFTER {
				self.open = false;
			}
		}
	}
}

/// The health of every backend.
#[derive(Default)]
pub struct Health {
	states: Mutex<HashMap<Backend, HealthState>>,
}

impl Health {
	pub fn record(&self, backend: Backend, ok: bool) {
		let mut states = self.states.lock().expect("poisoned");
		let state = states.entry(backend).or_default();
		let was = state.open;
		state.record(ok);
		if state.open != was {
			tracing::warn!("{:?} is {}", backend, if state.open { "healthy again" } else { "unhealthy" });
		}
	}

	pub fn is_open(&self, backend: Backend) -> bool {
		self.states.lock().expect("poisoned").get(&backend).is_some_and(|s| s.open)
	}

	/// The store invoice and the Lightning Address invoice need barkd and
	/// the Ark server.
	pub fn invoices(&self) -> bool {
		self.is_open(Backend::Barkd) && self.is_open(Backend::ArkServer)
	}

	/// LUD-22 `ARK` needs the Ark server, which the payer pays through.
	pub fn ark_payments(&self) -> bool {
		self.is_open(Backend::ArkServer)
	}

	/// On-chain sale addresses need bitcoind.
	pub fn onchain(&self) -> bool {
		self.is_open(Backend::Bitcoind)
	}

	pub fn snapshot(&self) -> HashMap<Backend, HealthState> {
		self.states.lock().expect("poisoned").clone()
	}
}

/// The protections of the service.
pub struct Protect {
	pub limits: Limits,
	trusted_proxies: IpList,
	allowlist: IpList,
	certification: Certification,
	certified_sources: IpList,
	windows: SlidingWindows,
	public_invoices: TokenBucket,
	pub health: Health,
}

impl Protect {
	pub fn new(limits: Limits, certification: Certification) -> anyhow::Result<Protect> {
		if certification.enabled && certification.token.as_deref().is_none_or(|t| t.len() < 16) {
			anyhow::bail!("certification needs a token of at least 16 characters");
		}
		Ok(Protect {
			trusted_proxies: IpList::parse(&limits.trusted_proxies)?,
			allowlist: IpList::parse(&limits.ip_allowlist)?,
			certified_sources: IpList::parse(&certification.allowed_sources)?,
			public_invoices: TokenBucket::new(limits.public_invoices_per_second),
			windows: SlidingWindows::default(),
			health: Health::default(),
			certification,
			limits,
		})
	}

	pub fn caller(&self, peer: IpAddr, headers: &HeaderMap) -> Caller {
		let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
		let ip = caller_ip(peer, xff, &self.trusted_proxies);
		let certified = self.certification.enabled
			&& self.certified_sources.contains(ip)
			&& headers.get(CERTIFICATION_HEADER).and_then(|v| v.to_str().ok())
				.zip(self.certification.token.as_deref())
				.is_some_and(|(given, token)| crate::token::hash(given) == crate::token::hash(token));
		Caller { ip, source: source_key(ip), bypass: self.allowlist.contains(ip) || certified }
	}

	fn limit_of(&self, surface: Surface) -> u32 {
		let l = &self.limits;
		match surface {
			Surface::Register => l.register_per_minute,
			Surface::Lookup => l.lookups_per_minute,
			Surface::Callback => l.callbacks_per_minute,
			Surface::SaleCreate => l.sales_per_minute,
			Surface::StatusRead => l.status_reads_per_minute,
			Surface::Owner => l.owner_per_minute,
		}
	}

	/// The per-source limit of a surface, per minute, in memory.
	pub fn check(&self, caller: &Caller, surface: Surface) -> ApiResult<()> {
		if caller.bypass {
			return Ok(());
		}
		let key = format!("{}:{}", surface.name(), caller.source);
		if self.windows.check(&key, self.limit_of(surface), Duration::from_secs(60), Instant::now()) {
			Ok(())
		} else {
			tracing::info!("rate limited: {} from {}", surface.name(), caller.source);
			Err(ApiError::TooManyRequests)
		}
	}

	/// Takes one new invoice from the public budget.
	pub fn take_public_invoice(&self) -> ApiResult<()> {
		if self.public_invoices.try_take(Instant::now()) {
			Ok(())
		} else {
			Err(ApiError::Upstream(anyhow::anyhow!("the public invoice budget is spent for this second")))
		}
	}

	pub fn sweep(&self) {
		let n = self.windows.sweep(Duration::from_secs(3600), Instant::now());
		if n > 0 {
			tracing::debug!("swept {} rate-limit keys", n);
		}
	}
}

/// Resolves the caller of every request and stores it in the request.
pub async fn resolve_caller(State(app): State<std::sync::Arc<App>>, mut req: Request, next: Next) -> Response {
	let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip())
		.unwrap_or(IpAddr::from([0, 0, 0, 0]));
	let caller = app.protect.caller(peer, req.headers());
	req.extensions_mut().insert(caller);
	next.run(req).await
}

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Caller {
	type Rejection = ApiError;

	async fn from_request_parts(parts: &mut axum::http::request::Parts, _: &S) -> Result<Self, Self::Rejection> {
		parts.extensions.get::<Caller>().cloned()
			.ok_or_else(|| ApiError::Internal(anyhow::anyhow!("no caller on the request")))
	}
}

/// Postgres sliding window: records a hit for `bucket` and fails when the
/// count in `window_secs` is above `limit`. An advisory lock on the bucket
/// serializes callers, so two cannot both pass. For limits shared by every
/// source, such as callbacks per nym.
pub async fn shared_window(
	conn: &mut tokio_postgres::Client,
	bucket: &str,
	limit: u32,
	window_secs: u32,
) -> ApiResult<()> {
	if limit == 0 {
		return Ok(());
	}
	let tx = conn.transaction().await?;
	tx.execute("SELECT pg_advisory_xact_lock(hashtext($1)::BIGINT)", &[&bucket]).await?;
	tx.execute("INSERT INTO rate_limit_event (bucket) VALUES ($1)", &[&bucket]).await?;
	let count: i64 = tx.query_one("
		SELECT count(*) FROM rate_limit_event
		WHERE bucket = $1 AND created_at > NOW() - make_interval(secs => $2)
	", &[&bucket, &(window_secs as f64)]).await?.get(0);
	tx.commit().await?;
	if count > limit as i64 {
		tracing::info!("rate limited: {}", bucket);
		return Err(ApiError::TooManyRequests);
	}
	Ok(())
}

/// Postgres distinct count: records that `source` used `name` and fails
/// when it used more than `limit` distinct names in `window_secs`. For
/// slow enumeration of names, and for one source registering many
/// identities.
pub async fn distinct_names(
	conn: &mut tokio_postgres::Client,
	kind: &str,
	source: &str,
	name: &str,
	limit: u32,
	window_secs: u32,
) -> ApiResult<()> {
	if limit == 0 {
		return Ok(());
	}
	let key = format!("{}:{}", kind, source);
	let tx = conn.transaction().await?;
	tx.execute("SELECT pg_advisory_xact_lock(hashtext($1)::BIGINT)", &[&key]).await?;
	tx.execute("INSERT INTO name_access_event (source_key, name) VALUES ($1, $2)", &[&key, &name]).await?;
	let count: i64 = tx.query_one("
		SELECT count(DISTINCT name) FROM name_access_event
		WHERE source_key = $1 AND created_at > NOW() - make_interval(secs => $2)
	", &[&key, &(window_secs as f64)]).await?.get(0);
	tx.commit().await?;
	if count > limit as i64 {
		tracing::info!("rate limited: distinct {} from {}", kind, source);
		return Err(ApiError::TooManyRequests);
	}
	Ok(())
}

/// Deletes the Postgres counters older than a day.
pub async fn sweep_shared(conn: &impl GenericClient) -> anyhow::Result<()> {
	conn.execute("DELETE FROM rate_limit_event WHERE created_at < NOW() - INTERVAL '1 day'", &[]).await?;
	conn.execute("DELETE FROM name_access_event WHERE created_at < NOW() - INTERVAL '1 day'", &[]).await?;
	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn ipv6_groups_by_56() {
		let a: IpAddr = "2001:db8:aa:bb01::1".parse().unwrap();
		let b: IpAddr = "2001:db8:aa:bbff:ffff::9".parse().unwrap();
		let c: IpAddr = "2001:db8:aa:bc00::1".parse().unwrap();
		assert_eq!(source_key(a), source_key(b), "same /56");
		assert_ne!(source_key(a), source_key(c), "another /56");
		assert_eq!(source_key("203.0.113.7".parse().unwrap()), "v4:203.0.113.7");
		assert_ne!(source_key("203.0.113.7".parse().unwrap()), source_key("203.0.113.8".parse().unwrap()));
	}

	#[test]
	fn ip_lists() {
		let l = IpList::parse(&["203.0.113.42".into(), "10.0.0.0/8".into(), "2001:db8::/32".into(), " ".into()]).unwrap();
		assert!(l.contains("203.0.113.42".parse().unwrap()));
		assert!(l.contains("10.5.1.1".parse().unwrap()));
		assert!(l.contains("2001:db8::1".parse().unwrap()));
		assert!(!l.contains("8.8.8.8".parse().unwrap()));
		assert!(IpList::parse(&["not-an-ip".into()]).is_err());
		assert!(IpList::parse(&["10.0.0.0/99".into()]).is_err());
	}

	#[test]
	fn forwarded_for_only_behind_a_trusted_proxy() {
		let proxies = IpList::parse(&["127.0.0.1".into()]).unwrap();
		let client: IpAddr = "203.0.113.7".parse().unwrap();
		// A direct caller cannot pose as another address.
		assert_eq!(caller_ip(client, Some("198.51.100.1"), &proxies), client);
		// Behind the proxy: the rightmost entry, which the proxy appended.
		let proxy: IpAddr = "127.0.0.1".parse().unwrap();
		assert_eq!(caller_ip(proxy, Some("198.51.100.1, 203.0.113.7"), &proxies), client);
		// A chain of trusted proxies is skipped.
		assert_eq!(caller_ip(proxy, Some("203.0.113.7, 127.0.0.1"), &proxies), client);
		// No header, or a malformed one: the proxy itself.
		assert_eq!(caller_ip(proxy, None, &proxies), proxy);
		assert_eq!(caller_ip(proxy, Some("garbage"), &proxies), proxy);
		// With no trusted proxy configured, the header is never read.
		assert_eq!(caller_ip(proxy, Some("203.0.113.7"), &IpList::default()), proxy);
	}

	#[test]
	fn sliding_windows() {
		let w = SlidingWindows::default();
		let t0 = Instant::now();
		for _ in 0..3 {
			assert!(w.check("k", 3, Duration::from_secs(60), t0));
		}
		assert!(!w.check("k", 3, Duration::from_secs(60), t0), "the fourth hit is refused");
		assert!(w.check("other", 3, Duration::from_secs(60), t0), "keys are separate");
		assert!(w.check("k", 3, Duration::from_secs(60), t0 + Duration::from_secs(61)), "the window slides");
		assert!(w.check("free", 0, Duration::from_secs(60), t0), "0 means no limit");
		assert_eq!(w.sweep(Duration::from_secs(10), t0 + Duration::from_secs(100)), 2, "a limit of 0 records nothing");
	}

	#[test]
	fn token_bucket() {
		let b = TokenBucket::new(2);
		let t0 = Instant::now();
		assert!(b.try_take(t0));
		assert!(b.try_take(t0));
		assert!(!b.try_take(t0));
		assert!(b.try_take(t0 + Duration::from_secs(1)));
	}

	#[test]
	fn health_hysteresis() {
		let mut h = HealthState::default();
		assert!(!h.open, "every backend starts closed");
		h.record(true);
		assert!(h.open, "the first success opens it");
		h.record(false);
		h.record(false);
		assert!(h.open, "two failures are not enough");
		h.record(false);
		assert!(!h.open, "three failures close it");
		h.record(true);
		assert!(!h.open, "one success does not reopen it");
		h.record(true);
		assert!(h.open, "two successes reopen it");
	}

	#[test]
	fn options_follow_their_backends() {
		let h = Health::default();
		assert!(!h.invoices() && !h.onchain() && !h.ark_payments());
		h.record(Backend::Barkd, true);
		h.record(Backend::Bitcoind, true);
		assert!(!h.invoices(), "an invoice needs the Ark server too");
		assert!(h.onchain());
		h.record(Backend::ArkServer, true);
		assert!(h.invoices() && h.ark_payments());
	}

	#[test]
	fn bypass() {
		let limits = Limits { ip_allowlist: vec!["10.0.0.0/8".into()], ..Limits::default() };
		let cert = Certification {
			enabled: true, token: Some("0123456789abcdef0123".into()), allowed_sources: vec!["192.0.2.1".into()],
		};
		let p = Protect::new(limits, cert).unwrap();
		let none = HeaderMap::new();
		assert!(p.caller("10.1.2.3".parse().unwrap(), &none).bypass);
		assert!(!p.caller("192.0.2.1".parse().unwrap(), &none).bypass);
		let mut certified = HeaderMap::new();
		certified.insert(CERTIFICATION_HEADER, "0123456789abcdef0123".parse().unwrap());
		assert!(p.caller("192.0.2.1".parse().unwrap(), &certified).bypass);
		assert!(!p.caller("192.0.2.2".parse().unwrap(), &certified).bypass, "token from another source");
		let mut wrong = HeaderMap::new();
		wrong.insert(CERTIFICATION_HEADER, "wrong".parse().unwrap());
		assert!(!p.caller("192.0.2.1".parse().unwrap(), &wrong).bypass);
		// A bypass caller skips the per-source limits.
		let c = p.caller("10.1.2.3".parse().unwrap(), &none);
		for _ in 0..100 {
			assert!(p.check(&c, Surface::Register).is_ok());
		}
		let c = p.caller("203.0.113.1".parse().unwrap(), &none);
		for _ in 0..5 {
			assert!(p.check(&c, Surface::Register).is_ok());
		}
		assert!(p.check(&c, Surface::Register).is_err(), "5 registrations per minute");
		// Certification without a usable token refuses to start.
		let bad = Certification { enabled: true, token: Some("short".into()), allowed_sources: vec![] };
		assert!(Protect::new(Limits::default(), bad).is_err());
	}
}
