// Copyright Materialize, Inc. and contributors. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Helpers for handling events from a Webhook source.

use std::collections::BTreeMap;
use std::sync::Arc;

use mz_adapter::{AdapterError, AppendWebhookError, AppendWebhookResponse};
use mz_ore::str::StrExt;
use mz_repr::adt::jsonb::JsonbPacker;
use mz_repr::{ColumnType, Datum, Row, ScalarType};
use mz_storage_client::controller::StorageError;

use anyhow::Context;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use bytes::Bytes;
use http::StatusCode;
use thiserror::Error;

/// The number of concurrent requests we allow at once for webhook sources.
///
/// TODO(parkmycar): Make this configurable via LaunchDarkly and/or as a setting on each webhook
/// source. The blocker for doing this today is an `axum` layer that allows us to __reduce__ the
/// concurrency limit, the current one only allows you to increase it.
pub const CONCURRENCY_LIMIT: usize = 100;

pub async fn handle_webhook(
    State(client): State<mz_adapter::Client>,
    Path((database, schema, name)): Path<(String, String, String)>,
    headers: http::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let conn_id = client.new_conn_id().context("allocate connection id")?;

    // Collect headers into a map, while converting them into strings.
    let mut headers_s = BTreeMap::new();
    for (name, val) in headers.iter() {
        if let Ok(val_s) = val.to_str().map(|s| s.to_string()) {
            // If a header is included more than once, bail returning an error to the user.
            let existing = headers_s.insert(name.as_str().to_string(), val_s);
            if existing.is_some() {
                let msg = format!("{} provided more than once", name.as_str());
                return Err(WebhookError::InvalidHeaders(msg));
            }
        }
    }
    let headers = Arc::new(headers_s);

    // Get an appender for the provided object, if that object exists.
    let AppendWebhookResponse {
        tx,
        body_ty,
        header_ty,
        validator,
    } = client
        .append_webhook(database, schema, name, conn_id)
        .await?;

    // If this source requires validation, then validate!
    if let Some(validator) = validator {
        let valid = validator
            .eval(Bytes::clone(&body), Arc::clone(&headers))
            .await?;
        if !valid {
            return Err(WebhookError::ValidationFailed);
        }
    }

    // Pack our body and headers into a Row.
    let row = pack_row(body, &headers, body_ty, header_ty)?;

    // Send the row to get appended.
    tx.append(vec![(row, 1)]).await?;

    Ok::<_, WebhookError>(())
}

/// Given the body and headers of a request, pack them into a [`Row`].
fn pack_row(
    body: Bytes,
    headers: &BTreeMap<String, String>,
    body_ty: ColumnType,
    header_ty: Option<ColumnType>,
) -> Result<Row, WebhookError> {
    // If we're including headers then we have two columns.
    let num_cols = header_ty.as_ref().map(|_| 2).unwrap_or(1);

    // Pack our row.
    let mut row = Row::with_capacity(num_cols);
    let mut packer = row.packer();

    // Pack our body into a row.
    match body_ty.scalar_type {
        ScalarType::Bytes => packer.push(Datum::Bytes(&body[..])),
        ty @ ScalarType::String => {
            let s = std::str::from_utf8(&body).map_err(|m| WebhookError::InvalidBody {
                ty,
                msg: m.to_string(),
            })?;
            packer.push(Datum::String(s));
        }
        ty @ ScalarType::Jsonb => {
            let jsonb_packer = JsonbPacker::new(&mut packer);
            jsonb_packer
                .pack_slice(&body[..])
                .map_err(|m| WebhookError::InvalidBody {
                    ty,
                    msg: m.to_string(),
                })?;
        }
        ty => {
            Err(anyhow::anyhow!(
                "Invalid body type for Webhook source: {ty:?}"
            ))?;
        }
    }

    // Pack the headers into our row, if required.
    if header_ty.is_some() {
        packer.push_dict(
            headers
                .iter()
                .map(|(name, val)| (name.as_str(), Datum::String(val))),
        );
    }

    Ok(row)
}

/// Errors we can encounter when appending data to a Webhook Source.
///
/// Webhook sources are a bit special since they are handled by `environmentd` (all other sources
/// are handled by `clusterd`) and data is "pushed" to them (all other source pull data). The
/// errors also generally need to map to HTTP status codes that we can use to respond to a webhook
/// request. As such, webhook errors don't cleanly map to any existing error type, hence the
/// existence of this error type.
#[derive(Error, Debug)]
pub enum WebhookError {
    #[error("no object was found at the path {}", .0.quoted())]
    NotFound(String),
    #[error("the required auth could not be found")]
    SecretMissing,
    #[error("this feature is currently unsupported: {0}")]
    Unsupported(&'static str),
    #[error("headers of request were invalid: {0}")]
    InvalidHeaders(String),
    #[error("failed to deserialize body as {ty:?}: {msg}")]
    InvalidBody { ty: ScalarType, msg: String },
    #[error("failed to validate the request")]
    ValidationFailed,
    #[error("error occurred while running validation")]
    ValidationError,
    #[error("internal storage failure! {0:?}")]
    InternalStorageError(StorageError),
    #[error("internal adapter failure! {0:?}")]
    InternalAdapterError(AdapterError),
    #[error("internal failure! {0:?}")]
    Internal(#[from] anyhow::Error),
}

impl From<StorageError> for WebhookError {
    fn from(err: StorageError) -> Self {
        match err {
            // TODO(parkmycar): Maybe map this to a HTTP 410 Gone instead of 404?
            StorageError::IdentifierMissing(id) | StorageError::IdentifierInvalid(id) => {
                WebhookError::NotFound(id.to_string())
            }
            e => WebhookError::InternalStorageError(e),
        }
    }
}

impl From<AdapterError> for WebhookError {
    fn from(err: AdapterError) -> Self {
        match err {
            AdapterError::Unsupported(feat) => WebhookError::Unsupported(feat),
            AdapterError::UnknownWebhookSource {
                database,
                schema,
                name,
            } => WebhookError::NotFound(format!("'{database}.{schema}.{name}'")),
            e => WebhookError::InternalAdapterError(e),
        }
    }
}

impl From<AppendWebhookError> for WebhookError {
    fn from(err: AppendWebhookError) -> Self {
        match err {
            AppendWebhookError::MissingSecret => WebhookError::SecretMissing,
            AppendWebhookError::ValidationError => WebhookError::ValidationError,
            AppendWebhookError::NonUtf8Body => WebhookError::InvalidBody {
                ty: ScalarType::String,
                msg: "invalid".to_string(),
            },
            AppendWebhookError::InternalError => {
                WebhookError::Internal(anyhow::anyhow!("failed to run validation"))
            }
        }
    }
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> axum::response::Response {
        match self {
            e @ WebhookError::NotFound(_) | e @ WebhookError::SecretMissing => {
                (StatusCode::NOT_FOUND, e.to_string()).into_response()
            }
            e @ WebhookError::Unsupported(_)
            | e @ WebhookError::InvalidBody { .. }
            | e @ WebhookError::ValidationFailed
            | e @ WebhookError::ValidationError => {
                (StatusCode::BAD_REQUEST, e.to_string()).into_response()
            }
            e @ WebhookError::InvalidHeaders(_) => {
                (StatusCode::UNAUTHORIZED, e.to_string()).into_response()
            }
            e @ WebhookError::InternalStorageError(StorageError::ResourceExhausted(_)) => {
                (StatusCode::TOO_MANY_REQUESTS, e.to_string()).into_response()
            }
            e @ WebhookError::InternalStorageError(_)
            | e @ WebhookError::InternalAdapterError(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
            WebhookError::Internal(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                e.root_cause().to_string(),
            )
                .into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use axum::response::IntoResponse;
    use bytes::Bytes;
    use http::StatusCode;
    use mz_adapter::AdapterError;
    use mz_repr::{ColumnType, GlobalId, ScalarType};
    use mz_storage_client::controller::StorageError;
    use proptest::prelude::*;

    use super::{pack_row, WebhookError};

    #[mz_ore::test]
    fn smoke_test_adapter_error_response_status() {
        // Unsupported errors get mapped to a certain response status.
        let resp = WebhookError::from(AdapterError::Unsupported("test")).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // All other errors should map to 500.
        let resp = WebhookError::from(AdapterError::Internal("test".to_string())).into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[mz_ore::test]
    fn smoke_test_storage_error_response_status() {
        // Resource exhausted should get mapped to a specific status code.
        let resp = WebhookError::from(StorageError::ResourceExhausted("test")).into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

        // IdentifierMissing should also get mapped to a specific status code.
        let resp =
            WebhookError::from(StorageError::IdentifierMissing(GlobalId::User(42))).into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // All other errors should map to 500.
        let resp = WebhookError::from(AdapterError::Internal("test".to_string())).into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[mz_ore::test]
    fn test_pack_invalid_column_type() {
        let body = Bytes::from(vec![42, 42, 42, 42]);
        let headers = BTreeMap::default();

        // Int64 is an invalid column type for a webhook source.
        let body_ty = ColumnType {
            scalar_type: ScalarType::Int64,
            nullable: false,
        };
        assert!(pack_row(body, &headers, body_ty, None).is_err());
    }

    proptest! {
        #[mz_ore::test]
        fn proptest_pack_row_never_panics(
            body: Vec<u8>,
            headers: BTreeMap<String, String>,
            body_ty: ColumnType,
            header_ty: Option<ColumnType>
        ) {
            let body = Bytes::from(body);
            // Call this method to make sure it doesn't panic.
            let _ = pack_row(body, &headers, body_ty, header_ty);
        }

        #[mz_ore::test]
        fn proptest_pack_row_succeeds_for_bytes(
            body: Vec<u8>,
            headers: BTreeMap<String, String>,
            include_headers: bool,
        ) {
            let body = Bytes::from(body);

            let body_ty = ColumnType { scalar_type: ScalarType::Bytes, nullable: false };
            let header_ty = include_headers.then(|| ColumnType {
                scalar_type: ScalarType::Map {
                    value_type: Box::new(ScalarType::String),
                    custom_id: None,
                },
                nullable: false,
            });

            prop_assert!(pack_row(body, &headers, body_ty, header_ty).is_ok());
        }

        #[mz_ore::test]
        fn proptest_pack_row_succeeds_for_strings(
            body: String,
            headers: BTreeMap<String, String>,
            include_headers: bool,
        ) {
            let body = Bytes::from(body);

            let body_ty = ColumnType { scalar_type: ScalarType::String, nullable: false };
            let header_ty = include_headers.then(|| ColumnType {
                scalar_type: ScalarType::Map {
                    value_type: Box::new(ScalarType::String),
                    custom_id: None,
                },
                nullable: false,
            });

            prop_assert!(pack_row(body, &headers, body_ty, header_ty).is_ok());
        }
    }
}
