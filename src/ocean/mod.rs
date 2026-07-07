mod charm;
pub mod erddap_client;
pub mod fusion;
mod hfr;
mod m1;
mod sst;
pub mod types;
mod upwelling;
mod wharf;

use std::sync::Arc;

use anyhow::Result;

use crate::biodiversity::BiodiversityService;
use crate::cache::CacheStore;
use erddap_client::ErddapClient;

pub use charm::HabRequest;
pub use fusion::{HabRiskRequest, ResearchSnapshotRequest, UpwellingStateRequest};
pub use hfr::HfrRequest;
pub use m1::M1Request;
pub use sst::SstRequest;
pub use upwelling::UpwellingRequest;
pub use wharf::WharfRequest;

pub struct OceanService {
    http: reqwest::Client,
    cache: Arc<CacheStore>,
    erddap: ErddapClient,
    biodiversity: Arc<BiodiversityService>,
}

impl OceanService {
    pub fn new(
        http: reqwest::Client,
        cache: Arc<CacheStore>,
        biodiversity: Arc<BiodiversityService>,
    ) -> Self {
        let erddap = ErddapClient::new(http.clone(), cache.clone());
        Self {
            http,
            cache,
            erddap,
            biodiversity,
        }
    }

    pub async fn get_upwelling_indices(&self, req: &UpwellingRequest) -> Result<String> {
        upwelling::fetch_and_format(&self.http, &self.cache, req).await
    }

    pub async fn get_m1_water_column(&self, req: &M1Request) -> Result<String> {
        let hours = req.hours.unwrap_or(24);
        let profile = req.include_profile.unwrap_or(true);
        let cache_key = format!("ocean:m1:h{}:p{}", hours, profile);
        self.cache
            .get_or_fetch(&cache_key, 1800, {
                let erddap = self.erddap.clone();
                let hours = req.hours;
                let profile = req.include_profile;
                move || async move {
                    let req = M1Request {
                        hours,
                        include_profile: profile,
                    };
                    m1::fetch_and_format(&erddap, &req).await
                }
            })
            .await
    }

    pub async fn get_sc_wharf_state(&self, req: &WharfRequest) -> Result<String> {
        let hours = req.hours.unwrap_or(6);
        let cache_key = format!("ocean:wharf:h{}", hours);
        self.cache
            .get_or_fetch(&cache_key, 300, {
                let erddap = self.erddap.clone();
                let hours = req.hours;
                move || async move {
                    let req = WharfRequest { hours };
                    wharf::fetch_and_format(&erddap, &req).await
                }
            })
            .await
    }

    pub async fn get_hab_risk_forecast(&self, req: &HabRequest) -> Result<String> {
        let cache_key = format!(
            "ocean:charm:{:.2}:{:.2}",
            req.lat.unwrap_or(36.96),
            req.lon.unwrap_or(-122.02),
        );
        self.cache
            .get_or_fetch(&cache_key, 3600, {
                let erddap = self.erddap.clone();
                let lat = req.lat;
                let lon = req.lon;
                let snap = req.snap_radius;
                move || async move {
                    let req = HabRequest {
                        lat,
                        lon,
                        snap_radius: snap,
                    };
                    charm::fetch_and_format(&erddap, &req).await
                }
            })
            .await
    }

    pub async fn get_sst_state(&self, req: &SstRequest) -> Result<String> {
        let cache_key = format!(
            "ocean:sst:{:.1}:{:.1}:{:.1}:{:.1}:s{}",
            req.lat_min.unwrap_or(36.5),
            req.lat_max.unwrap_or(37.2),
            req.lon_min.unwrap_or(-122.5),
            req.lon_max.unwrap_or(-121.8),
            req.stride.unwrap_or(2),
        );
        self.cache
            .get_or_fetch(&cache_key, 3600, {
                let erddap = self.erddap.clone();
                let lat_min = req.lat_min;
                let lat_max = req.lat_max;
                let lon_min = req.lon_min;
                let lon_max = req.lon_max;
                let stride = req.stride;
                move || async move {
                    let req = SstRequest {
                        lat_min,
                        lat_max,
                        lon_min,
                        lon_max,
                        stride,
                    };
                    sst::fetch_and_format(&erddap, &req).await
                }
            })
            .await
    }

    pub async fn get_hfradar_currents(&self, req: &HfrRequest) -> Result<String> {
        let cache_key = format!(
            "ocean:hfr:{}:{:.1}:{:.1}:{:.1}:{:.1}",
            req.resolution.as_deref().unwrap_or("6km"),
            req.lat_min.unwrap_or(36.5),
            req.lat_max.unwrap_or(37.2),
            req.lon_min.unwrap_or(-122.5),
            req.lon_max.unwrap_or(-121.8),
        );
        self.cache
            .get_or_fetch(&cache_key, 3600, {
                let erddap = self.erddap.clone();
                let resolution = req.resolution.clone();
                let lat_min = req.lat_min;
                let lat_max = req.lat_max;
                let lon_min = req.lon_min;
                let lon_max = req.lon_max;
                move || async move {
                    let req = HfrRequest {
                        resolution,
                        lat_min,
                        lat_max,
                        lon_min,
                        lon_max,
                    };
                    hfr::fetch_and_format(&erddap, &req).await
                }
            })
            .await
    }

    // ─── Fusion tools ───

    pub async fn monterey_bay_upwelling_state(
        &self,
        req: &fusion::UpwellingStateRequest,
    ) -> Result<String> {
        fusion::upwelling_state(
            &self.http,
            &self.cache,
            &self.erddap,
            &self.biodiversity,
            req,
        )
        .await
    }

    pub async fn monterey_bay_hab_risk(&self, req: &fusion::HabRiskRequest) -> Result<String> {
        fusion::hab_risk(
            &self.http,
            &self.cache,
            &self.erddap,
            &self.biodiversity,
            req,
        )
        .await
    }

    pub async fn monterey_bay_research_snapshot(
        &self,
        req: &fusion::ResearchSnapshotRequest,
    ) -> Result<String> {
        fusion::research_snapshot(
            &self.http,
            &self.cache,
            &self.erddap,
            &self.biodiversity,
            req,
        )
        .await
    }
}
