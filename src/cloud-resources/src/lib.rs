// Copyright Materialize, Inc. and contributors. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

// BEGIN LINT CONFIG
// DO NOT EDIT. Automatically generated by bin/gen-lints.
// Have complaints about the noise? See the note in misc/python/materialize/cli/gen-lints.py first.
#![allow(clippy::style)]
#![allow(clippy::complexity)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::mutable_key_type)]
#![allow(clippy::stable_sort_primitive)]
#![allow(clippy::map_entry)]
#![allow(clippy::box_default)]
#![warn(clippy::bool_comparison)]
#![warn(clippy::clone_on_ref_ptr)]
#![warn(clippy::no_effect)]
#![warn(clippy::unnecessary_unwrap)]
#![warn(clippy::dbg_macro)]
#![warn(clippy::todo)]
#![warn(clippy::wildcard_dependencies)]
#![warn(clippy::zero_prefixed_literal)]
#![warn(clippy::borrowed_box)]
#![warn(clippy::deref_addrof)]
#![warn(clippy::double_must_use)]
#![warn(clippy::double_parens)]
#![warn(clippy::extra_unused_lifetimes)]
#![warn(clippy::needless_borrow)]
#![warn(clippy::needless_question_mark)]
#![warn(clippy::needless_return)]
#![warn(clippy::redundant_pattern)]
#![warn(clippy::redundant_slicing)]
#![warn(clippy::redundant_static_lifetimes)]
#![warn(clippy::single_component_path_imports)]
#![warn(clippy::unnecessary_cast)]
#![warn(clippy::useless_asref)]
#![warn(clippy::useless_conversion)]
#![warn(clippy::builtin_type_shadow)]
#![warn(clippy::duplicate_underscore_argument)]
#![warn(clippy::double_neg)]
#![warn(clippy::unnecessary_mut_passed)]
#![warn(clippy::wildcard_in_or_patterns)]
#![warn(clippy::crosspointer_transmute)]
#![warn(clippy::excessive_precision)]
#![warn(clippy::overflow_check_conditional)]
#![warn(clippy::as_conversions)]
#![warn(clippy::match_overlapping_arm)]
#![warn(clippy::zero_divided_by_zero)]
#![warn(clippy::must_use_unit)]
#![warn(clippy::suspicious_assignment_formatting)]
#![warn(clippy::suspicious_else_formatting)]
#![warn(clippy::suspicious_unary_op_formatting)]
#![warn(clippy::mut_mutex_lock)]
#![warn(clippy::print_literal)]
#![warn(clippy::same_item_push)]
#![warn(clippy::useless_format)]
#![warn(clippy::write_literal)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::redundant_closure_call)]
#![warn(clippy::unnecessary_lazy_evaluations)]
#![warn(clippy::partialeq_ne_impl)]
#![warn(clippy::redundant_field_names)]
#![warn(clippy::transmutes_expressible_as_ptr_casts)]
#![warn(clippy::unused_async)]
#![warn(clippy::disallowed_methods)]
#![warn(clippy::disallowed_macros)]
#![warn(clippy::disallowed_types)]
#![warn(clippy::from_over_into)]
// END LINT CONFIG

//! Abstractions for management of cloud resources that have no equivalent when running
//! locally, like AWS PrivateLink endpoints.

use std::collections::BTreeMap;
use std::fmt::{self, Debug};
use std::sync::Arc;

use async_trait::async_trait;
use mz_repr::GlobalId;
use serde::{Deserialize, Serialize};

use crate::crd::vpc_endpoint::v1::VpcEndpointStatus;

pub mod crd;

/// A prefix for an [external ID] to use for all AWS AssumeRole operations. It
/// should be concatenanted with a non-user-provided suffix identifying the
/// source or sink. The ID used for the suffix should never be reused if the
/// source or sink is deleted.
///
/// **WARNING:** it is critical for security that this ID is **not** provided by
/// end users of Materialize. It must be provided by the operator of the
/// Materialize service.
///
/// This type protects against accidental construction of an
/// `AwsExternalIdPrefix` through the use of an unwieldy and overly descriptive
/// constructor method name.
///
/// [external ID]: https://docs.aws.amazon.com/IAM/latest/UserGuide/id_roles_create_for-user_externalid.html
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AwsExternalIdPrefix(String);

impl AwsExternalIdPrefix {
    /// Creates a new AWS external ID prefix from a command-line argument or
    /// an environment variable.
    ///
    /// **WARNING:** it is critical for security that this ID is **not**
    /// provided by end users of Materialize. It must be provided by the
    /// operator of the Materialize service.
    ///
    pub fn new_from_cli_argument_or_environment_variable(
        aws_external_id_prefix: &str,
    ) -> AwsExternalIdPrefix {
        AwsExternalIdPrefix(aws_external_id_prefix.into())
    }
}

impl fmt::Display for AwsExternalIdPrefix {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Configures a VPC endpoint.
pub struct VpcEndpointConfig {
    /// The name of the service to connect to.
    pub aws_service_name: String,
    /// The IDs of the availability zones in which the service is available.
    pub availability_zone_ids: Vec<String>,
}

#[async_trait]
pub trait CloudResourceController: CloudResourceReader {
    /// Creates or updates the specified `VpcEndpoint` Kubernetes object.
    async fn ensure_vpc_endpoint(
        &self,
        id: GlobalId,
        vpc_endpoint: VpcEndpointConfig,
    ) -> Result<(), anyhow::Error>;

    /// Deletes the specified `VpcEndpoint` Kubernetes object.
    async fn delete_vpc_endpoint(&self, id: GlobalId) -> Result<(), anyhow::Error>;

    /// Lists existing `VpcEndpoint` Kubernetes objects.
    async fn list_vpc_endpoints(
        &self,
    ) -> Result<BTreeMap<GlobalId, VpcEndpointStatus>, anyhow::Error>;

    /// Returns a reader for the resources managed by this controller.
    fn reader(&self) -> Arc<dyn CloudResourceReader>;
}

#[async_trait]
pub trait CloudResourceReader: Debug + Send + Sync {
    /// Reads the specified `VpcEndpoint` Kubernetes object.
    async fn read(&self, id: GlobalId) -> Result<VpcEndpointStatus, anyhow::Error>;
}

/// Returns the name to use for the VPC endpoint with the given ID.
pub fn vpc_endpoint_name(id: GlobalId) -> String {
    // This is part of the contract with the VpcEndpointController in the
    // cloud infrastructure layer.
    format!("connection-{id}")
}

/// Returns the host to use for the VPC endpoint with the given ID and
/// optionally in the given availability zone.
pub fn vpc_endpoint_host(id: GlobalId, availability_zone: Option<&str>) -> String {
    let name = vpc_endpoint_name(id);
    // This naming scheme is part of the contract with the VpcEndpointController
    // in the cloud infrastructure layer.
    match availability_zone {
        Some(az) => format!("{name}-{az}"),
        None => name,
    }
}
