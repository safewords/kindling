//! The built frontend.
//!
//! Rainier is the web server, so the files Vite writes need a route. Vite
//! content-hashes every filename, so a given URL's bytes never change — which
//! is what makes `immutable` safe here and would not be anywhere else.

use rainier_framework::prelude::*;
use rainier_framework::public::PublicFiles;

/// `GET /build/{path*}`
pub async fn build(request: Req) -> Result<Response> {
    // Resolved against `public`, and the route is mounted at `/build`, so the
    // request path already matches the directory layout on disk.
    let files = PublicFiles::at("public").cached_for("public, max-age=31536000, immutable");
    Ok(files.serve(&request).await)
}
