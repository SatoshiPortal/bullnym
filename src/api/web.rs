//! Serves the web app, embedded at build time (see `build.rs`).
//!
//! One page is the cashier screen and the public checkout: `/<nym>` for a
//! store's public link and `/c/<secret>` for its unlisted link. `/owner`
//! shows the owner's history, from a session token the owner's wallet
//! hands over. Viewing a page allocates nothing.

use axum::extract::Path;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;
use serde_json::json;

use crate::nym;


#[derive(RustEmbed)]
#[folder = "$OUT_DIR/web-dist/"]
struct WebDist;

fn serve_index() -> Response {
	match WebDist::get("index.html") {
		Some(file) => (
			[(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
			file.data,
		).into_response(),
		None => (StatusCode::NOT_FOUND, "bullnym was built without the web app").into_response(),
	}
}

/// The manifest of an install from a cashier link. Its `start_url` is the
/// link, so the installed app opens on it: on iOS the installed app does
/// not share storage with the browser that opened the link.
fn manifest(start_url: String) -> Response {
	let manifest = json!({
		"name": "Point of sale",
		"short_name": "POS",
		"start_url": start_url,
		"scope": "/",
		"display": "standalone",
		"background_color": "#f5f0e8",
		"theme_color": "#f5f0e8",
		"icons": [
			{ "src": "/icons/icon-192.png", "sizes": "192x192", "type": "image/png" },
			{ "src": "/icons/icon-512.png", "sizes": "512x512", "type": "image/png" },
		],
	});
	(
		[(header::CONTENT_TYPE, "application/manifest+json"), (header::CACHE_CONTROL, "no-cache")],
		manifest.to_string(),
	).into_response()
}

/// `GET /<nym>/manifest.webmanifest`.
pub async fn public_manifest(Path(name): Path<String>) -> Response {
	if nym::check_form(&name).is_err() {
		return StatusCode::NOT_FOUND.into_response();
	}
	manifest(format!("/{}", name))
}

/// `GET /c/<secret>/manifest.webmanifest`.
pub async fn unlisted_manifest(Path(secret): Path<String>) -> Response {
	if secret.is_empty() || secret.len() > 64 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
		return StatusCode::NOT_FOUND.into_response();
	}
	manifest(format!("/c/{}", secret))
}

/// Static assets. Unknown paths outside the API fall back to the web app,
/// so its routes (`/<nym>`, `/c/<secret>`, `/owner`) work on reload.
pub async fn asset(uri: Uri) -> Response {
	let path = uri.path().trim_start_matches('/');
	if path.starts_with("api/") || path.starts_with(".well-known/") {
		return StatusCode::NOT_FOUND.into_response();
	}
	if path.is_empty() {
		return serve_index();
	}
	match WebDist::get(path) {
		Some(file) => {
			// Vite hashes the names of bundled assets, so they never change.
			let cache = if path.starts_with("assets/") {
				"public, max-age=31536000, immutable"
			} else {
				"no-cache"
			};
			(
				[
					(header::CONTENT_TYPE, file.metadata.mimetype().to_string()),
					(header::CACHE_CONTROL, cache.to_string()),
				],
				file.data,
			).into_response()
		},
		None if path.starts_with("assets/") || path.contains('.') => StatusCode::NOT_FOUND.into_response(),
		None => serve_index(),
	}
}
