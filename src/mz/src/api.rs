// Copyright Materialize, Inc. and contributors. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::fmt::Display;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use mz_ore::retry::Retry;
use reqwest::{Client, Error};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::configuration::ValidProfile;
use crate::utils::RequestBuilderExt;

/// Cloud providers and regions available.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum CloudProviderRegion {
    #[serde(rename = "aws/us-east-1")]
    AwsUsEast1,
    #[serde(rename = "aws/eu-west-1")]
    AwsEuWest1,
}

/// Implementation to name the possible values and parse every option.
impl CloudProviderRegion {
    /// Return the region name inside a cloud provider.
    pub fn region_name(self) -> &'static str {
        match self {
            CloudProviderRegion::AwsUsEast1 => "us-east-1",
            CloudProviderRegion::AwsEuWest1 => "eu-west-1",
        }
    }
}

impl Display for CloudProviderRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudProviderRegion::AwsUsEast1 => write!(f, "aws/us-east-1"),
            CloudProviderRegion::AwsEuWest1 => write!(f, "aws/eu-west-1"),
        }
    }
}

impl FromStr for CloudProviderRegion {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "aws/us-east-1" => Ok(CloudProviderRegion::AwsUsEast1),
            "aws/eu-west-1" => Ok(CloudProviderRegion::AwsEuWest1),
            _ => bail!("Unknown region {}", s),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RegionInfo {
    pub sql_address: String,
    pub http_address: String,
    pub resolvable: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub region_info: Option<RegionInfo>,
}

impl RegionInfo {
    pub fn sql_url(&self, profile: &ValidProfile) -> Url {
        let mut url = Url::parse(&format!("postgres://{}", &self.sql_address))
            .expect("url known to be valid");
        url.set_username(profile.profile.get_email()).unwrap();
        url.set_path("materialize");
        if let Some(cert_file) = openssl_probe::probe().cert_file {
            url.query_pairs_mut()
                .append_pair("sslmode", "verify-full")
                .append_pair("sslrootcert", &cert_file.to_string_lossy());
        } else {
            url.query_pairs_mut().append_pair("sslmode", "require");
        }
        url
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CloudProviderResponse {
    pub data: Vec<CloudProvider>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CloudProvider {
    pub id: String,
    pub name: String,
    pub url: String,
    pub cloud_provider: String,
}

pub struct CloudProviderAndRegion {
    pub cloud_provider: CloudProvider,
    pub region: Option<Region>,
}

/// Enables a particular cloud provider's region
pub async fn enable_region(
    client: &Client,
    cloud_provider: &CloudProvider,
    version: Option<String>,
    environmentd_extra_args: Vec<String>,
    valid_profile: &ValidProfile<'_>,
) -> Result<Region, reqwest::Error> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Body {
        #[serde(skip_serializing_if = "Option::is_none")]
        environmentd_image_ref: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        environmentd_extra_args: Vec<String>,
    }

    let body = Body {
        environmentd_image_ref: version.map(|v| match v.split_once(':') {
            None => format!("materialize/environmentd:{v}"),
            Some((user, v)) => format!("{user}/environmentd:{v}"),
        }),
        environmentd_extra_args,
    };

    client
        .patch(format!("{:}/api/region", cloud_provider.url).as_str())
        .authenticate(&valid_profile.frontegg_auth)
        .json(&body)
        .send()
        .await?
        .json::<Region>()
        .await
}

/// Disables a particular cloud provider's region.
pub async fn disable_region(
    client: &Client,
    cloud_provider: &CloudProvider,
    valid_profile: &ValidProfile<'_>,
) -> Result<(), reqwest::Error> {
    Retry::default()
        .max_tries(100)
        .max_duration(Duration::from_secs(30))
        .retry_async(|_| async {
            client
                .delete(format!("{:}/api/region", cloud_provider.url).as_str())
                .authenticate(&valid_profile.frontegg_auth)
                .send()
                .await?
                .error_for_status()?;
            Ok(())
        })
        .await
}

//// Get a cloud provider's regions
pub async fn get_region(
    client: &Client,
    cloud_provider_region: &CloudProvider,
    valid_profile: &ValidProfile<'_>,
) -> Result<Region, anyhow::Error> {
    // Help us decode API responses that may be empty
    #[derive(Debug, Deserialize, Clone)]
    #[serde(untagged)]
    enum APIResponse {
        Empty,
        Region(Region),
    }

    let mut region_api_url = cloud_provider_region.url.clone();
    region_api_url.push_str("/api/region");

    let response = client
        .get(region_api_url)
        .authenticate(&valid_profile.frontegg_auth)
        .send()
        .await?;

    ensure!(response.status().is_success());
    match response.json::<APIResponse>().await? {
        APIResponse::Empty => bail!("The region is empty"),
        APIResponse::Region(region) => Ok(region),
    }
}

/// List all the available regions for a list of cloud providers.
pub async fn list_regions(
    cloud_providers: &Vec<CloudProvider>,
    client: &Client,
    valid_profile: &ValidProfile<'_>,
) -> Result<Vec<CloudProviderAndRegion>> {
    // TODO: Run requests in parallel
    let mut cloud_providers_and_regions: Vec<CloudProviderAndRegion> = Vec::new();

    for cloud_provider in cloud_providers {
        let possible_region = get_region(client, cloud_provider, valid_profile)
            .await
            .with_context(|| "Retrieving region details.");
        match possible_region {
            Ok(region) => cloud_providers_and_regions.push(CloudProviderAndRegion {
                cloud_provider: cloud_provider.clone(),
                region: Some(region.to_owned()),
            }),
            _ => cloud_providers_and_regions.push(CloudProviderAndRegion {
                cloud_provider: cloud_provider.clone(),
                region: None,
            }),
        }
    }

    Ok(cloud_providers_and_regions)
}

/// List all the available cloud providers.
///
/// E.g.: [us-east-1, eu-west-1]
pub async fn list_cloud_providers(
    client: &Client,
    valid_profile: &ValidProfile<'_>,
) -> Result<CloudProviderResponse, Error> {
    client
        .get(valid_profile.profile.endpoint().cloud_regions_url())
        .authenticate(&valid_profile.frontegg_auth)
        .send()
        .await?
        .json::<CloudProviderResponse>()
        .await
}

pub async fn get_cloud_provider(
    client: &Client,
    valid_profile: &ValidProfile<'_>,
    cloud_provider_region: &CloudProviderRegion,
) -> Result<CloudProvider> {
    let cloud_providers = list_cloud_providers(client, valid_profile)
        .await
        .with_context(|| "Retrieving cloud providers.")?;

    // Create a vec with only one region
    let cloud_provider: CloudProvider = cloud_providers
        .data
        .into_iter()
        .find(|provider| provider.name == cloud_provider_region.region_name())
        .with_context(|| "Retriving cloud provider from list.")?;

    Ok(cloud_provider)
}

pub async fn get_region_info_by_cloud_provider(
    client: &Client,
    valid_profile: &ValidProfile<'_>,
    cloud_provider_region: &CloudProviderRegion,
) -> Result<RegionInfo> {
    let cloud_provider = get_cloud_provider(client, valid_profile, cloud_provider_region)
        .await
        .with_context(|| "Retrieving cloud provider.")?;

    let region = get_region(client, &cloud_provider, valid_profile)
        .await
        .with_context(|| "Retrieving region.")?;

    region
        .region_info
        .with_context(|| "Retrieving region info.")
}
