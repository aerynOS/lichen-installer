// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Fetching a model document from a URI.
//!
//! Four schemes: https needs a TLS client the workspace does not otherwise carry,
//! and smb/nfs need a mount, which is privileged. Local paths ride along so that
//! one RPC covers every case and the frontend never has to branch on scheme.

use std::fs;
use tonic::Status;

/// A model document is a few kilobytes, anything past this is not one.
const MAX_DOCUMENT: u64 = 1024 * 1024;

/// The scheme of a URI, or None for a bare path.
fn scheme(uri: &str) -> Option<&str> {
    uri.split_once("://").map(|(scheme, _)| scheme)
}

/// Read a local file. `file://` is accepted, but not required: the picker
/// hands back plain paths, and so does anyone typing one for an external drive.
fn local(path: &str) -> Result<String, Status> {
    let path = path.strip_prefix("file://").unwrap_or(path);

    if !path.starts_with('/') {
        return Err(Status::invalid_argument(format!(
            "{path} is not an absolute path (a file:// URI needs three slashes)"
        )));
    }

    let length = fs::metadata(path)
        .map_err(|e| Status::not_found(format!("cannot read {path}: {e}")))?
        .len();

    if length > MAX_DOCUMENT {
        return Err(Status::out_of_range(format!(
            "{path} is {length} bytes: a model document is not that large"
        )));
    }

    fs::read_to_string(path).map_err(|e| Status::not_found(format!("cannot read {path}: {e}")))
}

/// Fetch over HTTP(S).
///
/// Bounded twice: once the advertised length, which costs nothing and stops
/// the common case, and once on what actually arrived, because a server is
/// free to lie or say nothing at all.
async fn remote(uri: &str) -> Result<String, Status> {
    let response = reqwest::get(uri)
        .await
        .map_err(|e| Status::unavailable(format!("cannot reach {uri}: {e}")))?;

    if !response.status().is_success() {
        return Err(Status::not_found(format!("{uri} returned {}", response.status())));
    }

    if let Some(length) = response.content_length()
        && length > MAX_DOCUMENT
    {
        return Err(Status::out_of_range(format!(
            "{uri} is {length} bytes; a model document is not that large"
        )));
    }

    let body = response
        .bytes()
        .await
        .map_err(|e| Status::unavailable(format!("{uri} stopped sending: {e}")))?;

    if body.len() as u64 > MAX_DOCUMENT {
        return Err(Status::out_of_range(format!(
            "{uri} sent {} bytes; a model document is not that large",
            body.len()
        )));
    }

    String::from_utf8(body.to_vec())
        .map_err(|_| Status::invalid_argument(format!("{uri} is not valid UTF-8, so it is not a model document")))
}

/// Resolve a URI to a document, whatever kind of URI it is.
pub(super) async fn fetch(uri: &str) -> Result<String, Status> {
    match scheme(uri) {
        None | Some("file") => local(uri),
        Some("http" | "https") => remote(uri).await,
        Some(other) => Err(Status::invalid_argument(format!(
            "unsupported scheme {other:?}: use a path, file://, https:// smb://, or nfs://"
        ))),
    }
}
